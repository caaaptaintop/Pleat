//! Direct skill integrations verified against Oh My Pi's source and DimAgent
//! 0.5.16's published skill-loader. Installation stays separate from discovery.
use super::*;

pub(super) struct OmpAdapter;
pub(super) struct DimAgentAdapter {
    pub(super) home: PathBuf,
}

fn json_file(path: &Path) -> serde_json::Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn omp_settings(context: &DiscoveryContext) -> serde_json::Value {
    let mut merged = serde_json::Value::Object(Default::default());
    let mut dirs = vec![crate::agents::omp_agent_dir(&context.home)];
    if !is_global_context(context) {
        // Only the nearest project config participates in OMP settings.
        if let Some(dir) = cwd_to_boundary(&context.cwd, context.repository_root.as_deref())
            .into_iter()
            .find(|dir| dir.join(".omp").is_dir())
        {
            dirs.push(dir.join(".omp"));
        }
    }
    for dir in dirs {
        for file in ["config.yaml", "config.yml", "settings.json"] {
            let Some(value) = fs::read_to_string(dir.join(file))
                .ok()
                .and_then(|s| serde_yaml::from_str::<serde_json::Value>(&s).ok())
            else {
                continue;
            };
            merge_settings(&mut merged, value);
        }
    }
    merged
}

fn merge_settings(dest: &mut serde_json::Value, source: serde_json::Value) {
    if let (Some(dest), Some(source)) = (dest.as_object_mut(), source.as_object()) {
        for (key, value) in source {
            if value.is_object() {
                merge_settings(
                    dest.entry(key).or_insert_with(|| serde_json::json!({})),
                    value.clone(),
                );
            } else {
                dest.insert(key.clone(), value.clone());
            }
        }
    }
}

fn setting_enabled(settings: &serde_json::Value, key: &str) -> bool {
    settings.pointer(&format!("/skills/{key}")) != Some(&serde_json::Value::Bool(false))
}

fn project_dirs(context: &DiscoveryContext) -> Vec<PathBuf> {
    if is_global_context(context) {
        return Vec::new();
    }
    cwd_to_boundary(
        &context.cwd,
        context.repository_root.as_deref().or(Some(&context.home)),
    )
    .into_iter()
    .filter(|dir| dir != &context.home)
    .collect()
}

impl AgentSkillPolicy for OmpAdapter {
    fn agent(&self) -> AgentKind {
        AgentKind::Omp
    }
    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
        let settings = omp_settings(context);
        if !setting_enabled(&settings, "enabled") {
            return Vec::new();
        }
        let dirs = project_dirs(context);
        let mut roots = Vec::new();
        // Custom directories override conventional provider roots.
        for path in settings
            .pointer("/skills/customDirectories")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
        {
            roots.push(
                SkillRoot::new(
                    resolve_agent_path(path, &context.home, &context.cwd),
                    SkillScope::Local,
                )
                .direct_children(),
            );
        }
        for (suffix, user_path, project_toggle, user_toggle) in [
            (
                ".omp/skills",
                crate::agents::omp_agent_dir(&context.home).join("skills"),
                "enablePiProject",
                "enablePiUser",
            ),
            (
                ".claude/skills",
                context.home.join(".claude/skills"),
                "enableClaudeProject",
                "enableClaudeUser",
            ),
            (
                ".agent/skills",
                context.home.join(".agent/skills"),
                "enableAgentsProject",
                "enableAgentsUser",
            ),
            (
                ".agents/skills",
                context.home.join(".agents/skills"),
                "enableAgentsProject",
                "enableAgentsUser",
            ),
            (
                ".codex/skills",
                codex_home(context).join("skills"),
                "enableCodexProject",
                "enableCodexUser",
            ),
            (
                ".opencode/skills",
                context.home.join(".config/opencode/skills"),
                "enableOpenCodeProject",
                "enableOpenCodeUser",
            ),
            (
                ".github/skills",
                PathBuf::new(),
                "enableGithubProject",
                "enableGithubUser",
            ),
        ] {
            if setting_enabled(&settings, project_toggle) {
                roots.extend(dirs.iter().map(|dir| {
                    SkillRoot::new(dir.join(suffix), SkillScope::Repository).direct_children()
                }));
            }
            let provider = suffix
                .split('/')
                .next()
                .unwrap_or_default()
                .trim_start_matches('.');
            let user_opted_in = matches!(provider, "omp" | "agent" | "agents")
                || settings
                    .get("enabledProviders")
                    .and_then(|v| v.as_array())
                    .is_some_and(|values| values.iter().any(|v| v.as_str() == Some(provider)))
                || settings
                    .pointer(&format!("/skills/{user_toggle}"))
                    .and_then(|v| v.as_bool())
                    == Some(true);
            if !user_path.as_os_str().is_empty()
                && user_opted_in
                && setting_enabled(&settings, user_toggle)
            {
                roots.push(SkillRoot::new(user_path, SkillScope::User).direct_children());
            }
        }
        roots
    }
    fn is_enabled_for_root(
        &self,
        root: &SkillRoot,
        _: &Path,
        metadata: &SkillMetadata,
        context: &DiscoveryContext,
    ) -> bool {
        let fm = skill_frontmatter_value(metadata);
        let requires_description = root.path.ends_with(".omp/skills")
            || root.path == crate::agents::omp_agent_dir(&context.home).join("skills")
            || root.path.ends_with(".github/skills")
            || ![
                ".claude/skills",
                ".agent/skills",
                ".agents/skills",
                ".codex/skills",
                ".opencode/skills",
            ]
            .iter()
            .any(|s| root.path.ends_with(s));
        if fm.get("enabled").and_then(|v| v.as_bool()) == Some(false)
            || metadata.name.contains(['/', '\\'])
            || (requires_description
                && (!metadata.has_explicit_description || metadata.description.trim().is_empty()))
        {
            return false;
        }
        let settings = omp_settings(context);
        let matches_list = |key: &str, prefix: &str| {
            settings
                .pointer(key)
                .and_then(|v| v.as_array())
                .is_some_and(|values| {
                    values.iter().filter_map(|v| v.as_str()).any(|pattern| {
                        wildcard_match(pattern, &format!("{prefix}{}", metadata.name))
                    })
                })
        };
        !matches_list("/disabledExtensions", "skill:") && !matches_list("/skills/ignoredSkills", "")
    }
    fn name_collision(&self) -> NameCollision {
        NameCollision::KeepAll
    }
    fn visibility(
        &self,
        _: &Path,
        metadata: &SkillMetadata,
        _: &DiscoveryContext,
    ) -> SkillVisibility {
        let fm = skill_frontmatter_value(metadata);
        if ["hide", "disable-model-invocation", "disableModelInvocation"]
            .iter()
            .any(|key| fm.get(*key).and_then(|v| v.as_bool()) == Some(true))
        {
            SkillVisibility::ManualOnly
        } else {
            SkillVisibility::Automatic
        }
    }
    fn prompt_path_for_entry(
        &self,
        _: &SkillRoot,
        _: &Path,
        _: &Path,
        metadata: &SkillMetadata,
        _: &DiscoveryContext,
    ) -> Option<String> {
        Some(format!("skill://{}", metadata.name))
    }
    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        skills
            .iter()
            .map(|s| format!("- {}: {}", s.name, s.description))
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn estimate_tokens(&self, rendered: &str) -> usize {
        approx_token_count(rendered)
    }
}

fn skill_frontmatter_value(metadata: &SkillMetadata) -> serde_yaml::Value {
    fs::read_to_string(&metadata.source_path)
        .ok()
        .and_then(|s| yaml_frontmatter(&s).and_then(|fm| serde_yaml::from_str(fm).ok()))
        .unwrap_or_default()
}

pub(super) fn dim_home(context: &DiscoveryContext) -> PathBuf {
    env::var("DIMCODE_HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| resolve_agent_path(v.trim(), &context.home, &context.cwd))
        .unwrap_or_else(|| {
            env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| context.home.clone())
                .join(".dimcode/v2")
        })
}

impl AgentSkillPolicy for DimAgentAdapter {
    fn agent(&self) -> AgentKind {
        AgentKind::DimAgent
    }
    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
        // DimAgent scans host/user sources first; same-name workspace skills
        // are shadowed even when their user-level counterpart is disabled.
        let mut roots = vec![
            SkillRoot::new(self.home.join("skills"), SkillScope::User),
            SkillRoot::new(context.home.join(".agents/skills"), SkillScope::User),
        ];
        if !is_global_context(context) {
            roots.push(SkillRoot::new(
                context.cwd.join(".agents/skills"),
                SkillScope::Local,
            ));
        }
        roots
    }
    fn is_enabled(&self, _: &Path, metadata: &SkillMetadata, _: &DiscoveryContext) -> bool {
        let fm = skill_frontmatter_value(metadata);
        fm.get("name")
            .and_then(|v| v.as_str())
            .is_some_and(|v| !v.trim().is_empty())
            && metadata.has_explicit_description
            && !metadata.description.trim().is_empty()
            && fs::read_to_string(&metadata.source_path)
                .is_ok_and(|s| !first_body_paragraph(&s).is_empty())
    }
    fn name_collision(&self) -> NameCollision {
        NameCollision::FirstWins
    }
    fn visibility_for_root(
        &self,
        root: &SkillRoot,
        dir: &Path,
        _: &SkillMetadata,
        _: &DiscoveryContext,
    ) -> SkillVisibility {
        let relative = dir
            .strip_prefix(&root.path)
            .unwrap_or(dir)
            .to_string_lossy()
            .replace('\\', "/");
        let id = if root.scope == SkillScope::Local {
            format!("workspace:{}:{relative}", root.path.display())
        } else {
            format!("user:{relative}")
        };
        if json_file(&self.home.join("skills.json"))
            .get("disabledSkillIds")
            .and_then(|v| v.as_array())
            .is_some_and(|values| values.iter().any(|v| v.as_str() == Some(&id)))
        {
            SkillVisibility::ManualOnly
        } else {
            SkillVisibility::Automatic
        }
    }
    fn visibility(&self, _: &Path, _: &SkillMetadata, _: &DiscoveryContext) -> SkillVisibility {
        SkillVisibility::Automatic
    }
    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        let mut sorted = skills.iter().collect::<Vec<_>>();
        sorted.sort_by_key(|s| &s.name);
        let mut costs = sorted.clone();
        costs.sort_by_key(|s| s.description.chars().count());
        let mut budget = 4_000;
        let mut full = HashSet::new();
        for skill in costs {
            let cost = skill.description.chars().count() + 2;
            if cost <= budget {
                budget -= cost;
                full.insert(&skill.name);
            }
        }
        sorted
            .iter()
            .map(|s| {
                if full.contains(&s.name) {
                    format!("- {}: {}", s.name, s.description)
                } else {
                    format!("- {}", s.name)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn estimate_tokens(&self, rendered: &str) -> usize {
        approx_token_count(rendered)
    }
}

/// Distinct same-name variants remain addressable in OMP; physical copies
/// of the same skill are emitted only once.
pub(super) fn resolve_omp_collisions(
    skills: Vec<EffectiveSkill>,
    context: &DiscoveryContext,
) -> Vec<EffectiveSkill> {
    let mut result = Vec::new();
    let mut paths = HashSet::new();
    let mut names = HashSet::new();
    let mut contents = HashMap::new();
    for mut skill in skills {
        if !paths.insert(fs::canonicalize(&skill.path).unwrap_or_else(|_| skill.path.clone())) {
            continue;
        }
        let content = fs::read_to_string(&skill.path).unwrap_or_default();
        if let Some(previous) = contents.get(&skill.name) {
            if previous == &content {
                continue;
            }
            let namespace = skill
                .root_path
                .as_ref()
                .and_then(|root| root.parent())
                .and_then(|dir| dir.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("omp")
                .trim_start_matches('.');
            let base = format!("{namespace}/{}", skill.name);
            let mut name = base.clone();
            let mut suffix = 2;
            while names.contains(&name) {
                name = format!("{base}~{suffix}");
                suffix += 1;
            }
            skill.id = name.clone();
            skill.name = name.clone();
            skill.prompt_path = Some(format!("skill://{name}"));
        } else {
            contents.insert(skill.name.clone(), content);
        }
        names.insert(skill.name.clone());
        result.push(skill);
    }
    let settings = omp_settings(context);
    let matches = |key: &str, value: &str| {
        settings
            .pointer(key)
            .and_then(|v| v.as_array())
            .is_some_and(|values| {
                values
                    .iter()
                    .filter_map(|v| v.as_str())
                    .any(|pattern| wildcard_match(pattern, value))
            })
    };
    result.retain(|skill| {
        !matches("/disabledExtensions", &format!("skill:{}", skill.name))
            && !matches("/skills/ignoredSkills", &skill.name)
            && (settings
                .pointer("/skills/includeSkills")
                .and_then(|v| v.as_array())
                .is_none_or(|v| v.is_empty())
                || matches("/skills/includeSkills", &skill.name))
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_skill(root: &Path, name: &str, extra: &str) {
        fs::create_dir_all(root.join(name)).unwrap();
        fs::write(
            root.join(name).join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: Use {name}\n{extra}---\nSkill instructions.\n"
            ),
        )
        .unwrap();
    }
    fn context(home: PathBuf, cwd: PathBuf, repo: PathBuf) -> DiscoveryContext {
        DiscoveryContext {
            home,
            cwd,
            repository_root: Some(repo),
        }
    }
    fn estimate(context: &DiscoveryContext, agent: AgentKind) -> AgentContextEstimate {
        if agent == AgentKind::DimAgent {
            return adapters::inspect_dimagent(context, context.home.join(".dimcode/v2"));
        }
        adapters::inspect_project(context, false, crate::agents::AGENT_ICON_ORDER)
            .into_iter()
            .find(|s| s.agent == agent)
            .unwrap()
    }

    #[test]
    fn omp_project_install_round_trip_and_direct_discovery() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        let cwd = repo.join("child");
        fs::create_dir_all(&cwd).unwrap();
        let library = temp.path().join("library");
        write_skill(&library, "installed", "");
        crate::project::install(&repo, &library, "installed", &[crate::InstallTarget::Omp])
            .unwrap();
        write_skill(&repo.join(".omp/skills/group"), "nested", "");
        write_skill(&repo.join(".omp/skills"), ".hidden", "");
        let context = context(home, cwd, repo.clone());
        let result = estimate(&context, AgentKind::Omp);
        assert_eq!(
            result
                .skills
                .iter()
                .filter(|s| s.name == "installed")
                .count(),
            1
        );
        assert!(
            !result
                .skills
                .iter()
                .any(|s| s.name == "nested" || s.name == ".hidden")
        );
        assert!(
            crate::project::list(&repo, &library)
                .unwrap()
                .iter()
                .find(|s| s.name == "installed")
                .unwrap()
                .installations[0]
                .managed
        );
        crate::project::uninstall(&repo, &library, "installed", &[crate::InstallTarget::Omp])
            .unwrap();
        assert!(library.join("installed/SKILL.md").is_file());
        assert!(!repo.join(".omp/skills/installed").exists());
    }

    #[test]
    fn omp_settings_filter_hidden_disabled_and_custom_skills() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        write_skill(&repo.join(".omp/skills"), "hidden", "hide: true\n");
        write_skill(&repo.join(".omp/skills"), "disabled", "enabled: false\n");
        write_skill(&repo.join(".agents/skills"), "shared", "");
        write_skill(&repo.join("custom"), "custom", "");
        fs::write(
            repo.join(".omp/config.yml"),
            "skills:\n  customDirectories: [custom]\n  ignoredSkills: [shared]\n",
        )
        .unwrap();
        let result = estimate(&context(home, repo.clone(), repo), AgentKind::Omp);
        assert!(result.skills.iter().any(|s| s.name == "custom"));
        assert!(
            result
                .skills
                .iter()
                .any(|s| s.name == "hidden" && s.visibility == SkillVisibility::ManualOnly)
        );
        assert!(
            !result
                .skills
                .iter()
                .any(|s| s.name == "disabled" || s.name == "shared")
        );
    }

    #[test]
    fn omp_keeps_distinct_collisions_and_deduplicates_linked_sources() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        write_skill(&repo.join(".omp/skills"), "same", "");
        write_skill(&repo.join(".agents/skills"), "same", "hide: true\n");
        write_skill(&repo.join(".omp/skills"), "linked", "");
        crate::directory_link::create(
            &repo.join(".omp/skills/linked"),
            &repo.join(".agents/skills/linked"),
        )
        .unwrap();
        let result = estimate(&context(home, repo.clone(), repo), AgentKind::Omp);
        assert!(result.skills.iter().any(|s| s.name == "same"));
        assert!(result.skills.iter().any(|s| s.name == "agents/same"));
        assert_eq!(
            result.skills.iter().filter(|s| s.name == "linked").count(),
            1
        );
    }

    #[test]
    fn dimagent_uses_shared_skills_user_first_and_only_current_workspace() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        let cwd = repo.join("child");
        write_skill(&home.join(".agents/skills"), "same", "");
        write_skill(&cwd.join(".agents/skills"), "same", "");
        write_skill(
            &cwd.join(".agents/skills/group"),
            "nested",
            "disable-model-invocation: true\n",
        );
        write_skill(&repo.join(".agents/skills"), "parent", "");
        let result = estimate(
            &context(home.clone(), cwd.clone(), repo),
            AgentKind::DimAgent,
        );
        assert_eq!(result.skills.iter().filter(|s| s.name == "same").count(), 2);
        assert_eq!(result.skills.iter().filter(|s| s.name == "same" && s.visibility != SkillVisibility::Shadowed).count(), 1);
        assert_eq!(
            result
                .skills
                .iter()
                .find(|s| s.name == "same")
                .unwrap()
                .scope,
            SkillScope::User
        );
        assert!(
            result
                .skills
                .iter()
                .any(|s| s.name == "nested" && s.visibility == SkillVisibility::Automatic)
        );
        assert!(!result.skills.iter().any(|s| s.name == "parent"));
    }
    #[test]
    fn dimagent_disabled_user_skill_still_shadows_the_workspace() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        write_skill(&home.join(".agents/skills"), "same", "");
        write_skill(&repo.join(".agents/skills"), "same", "");
        let context = context(home, repo.clone(), repo);
        let state = context.home.join(".dimcode/v2");
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join("skills.json"),
            r#"{"disabledSkillIds":["user:same"]}"#,
        )
        .unwrap();
        let result = estimate(&context, AgentKind::DimAgent);
        assert_eq!(result.model_visible_count, 0);
        assert!(result.skills.iter().filter(|s| s.name == "same").all(|s| s.visibility == SkillVisibility::Shadowed));
    }

    #[test]
    fn missing_descriptions_follow_each_agents_validation_rules() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        for root in [".omp/skills", ".agents/skills"] {
            let dir = repo.join(root).join("no-description");
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("SKILL.md"),
                "---\nname: no-description\n---\nBody.\n",
            )
            .unwrap();
        }
        let context = context(home, repo.clone(), repo);
        let omp = estimate(&context, AgentKind::Omp);
        let skill = omp
            .skills
            .iter()
            .find(|s| s.name == "no-description")
            .unwrap();
        assert!(skill.path.to_string_lossy().contains(".agents/skills"));
        assert!(skill.description.is_empty());
        assert!(estimate(&context, AgentKind::DimAgent).skills.is_empty());
    }
}
