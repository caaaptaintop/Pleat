//! Codex 0.163 static User-layer rules; no host execution or session overrides.

use super::*;
use crate::config_read::same_regular_file;

/// The existing context represents the default .git boundary, not a native
/// merged config stack (custom markers, profiles and SessionFlags are absent).
pub(super) fn roots(context: &DiscoveryContext, user: &Path) -> Vec<SkillRoot> {
    // The trait can return only roots. Public estimates use the fallible
    // snapshot below and preserve diagnostics before any roots are scanned.
    user_config(user, &context.home, || {}).map(|snapshot| {
        roots_from_config(context, user, &snapshot.config)
    }).unwrap_or_default()
}

fn roots_from_config(context: &DiscoveryContext, user: &Path, config: &toml::Value) -> Vec<SkillRoot> {
    let directories = if is_global_context(context) {
        Vec::new()
    } else {
        let boundary = context.repository_root.as_deref()
            .filter(|root| context.cwd.starts_with(root)).unwrap_or(&context.cwd);
        cwd_to_boundary(&context.cwd, Some(boundary))
    };
    // Project layers are high to low: cwd -> project root, including middle
    // .codex folders with no config.toml. Every native Project root is Repo.
    let mut roots = directories.iter().map(|dir| {
        SkillRoot::new(dir.join(".codex/skills"), SkillScope::Repository)
    }).collect::<Vec<_>>();
    roots.push(SkillRoot::new(user.join("skills"), SkillScope::User));
    roots.push(SkillRoot::new(context.home.join(".agents/skills"), SkillScope::User));
    roots.push(SkillRoot::new(user.join("skills/.system"), SkillScope::System)
        .without_directory_symlinks());
    #[cfg(unix)]
    roots.push(SkillRoot::new(PathBuf::from("/etc/codex/skills"), SkillScope::System));
    roots.extend(codex_plugin_roots(user, config));
    // Native repo .agents roots are appended root -> cwd, after config roots.
    roots.extend(directories.iter().rev().map(|dir| {
        SkillRoot::new(dir.join(".agents/skills"), SkillScope::Repository)
    }));
    roots
}

#[derive(Debug, PartialEq, Eq)]
enum Selector { Path(PathBuf), Name(String) }

#[derive(Debug, PartialEq, Eq)]
struct Rule { selector: Selector, enabled: bool }

const MAX_RULES: usize = 1024;

struct UserConfig { config: toml::Value, rules: Vec<Rule> }

#[derive(Deserialize)]
struct ConfigEntry {
    path: Option<PathBuf>,
    name: Option<String>,
    // Required in 0.163 SkillConfig; only bundled.enabled defaults to true.
    enabled: bool,
}

#[derive(Default, Deserialize)]
struct SkillsConfig {
    #[serde(default)]
    config: Vec<ConfigEntry>,
    // Validate the remaining native SkillsConfig fields without applying
    // controls whose effective values need the unavailable merged stack.
    #[serde(rename = "bundled")]
    _bundled: Option<BundledConfig>,
    #[serde(rename = "include_instructions")]
    _include_instructions: Option<bool>,
    #[serde(rename = "max_context_tokens")]
    _max_context_tokens: Option<std::num::NonZeroUsize>,
}

#[derive(Deserialize)]
struct BundledConfig {
    #[serde(default = "default_enabled", rename = "enabled")]
    _enabled: bool,
}

fn default_enabled() -> bool { true }

fn parse_rules(config: &toml::Value, user: &Path, home: &Path) -> Result<Vec<Rule>, &'static str> {
    let Some(value) = config.get("skills") else { return Ok(Vec::new()); };
    // Charge raw entries, including repeated or invalid selectors, before the
    // typed Vec allocation, path I/O and ordered retain pass.
    if value.get("config").and_then(toml::Value::as_array).is_some_and(|entries| entries.len() > MAX_RULES) {
        return Err("codex_user_config_rule_limit");
    }
    let config = SkillsConfig::deserialize(value.clone()).map_err(|_| "codex_user_config_invalid")?;
    let mut rules = Vec::<Rule>::new();
    for entry in config.config {
        let selector = match (entry.path, entry.name) {
            (Some(path), None) => {
                let path = resolve_selector_path(&path, user, home)
                    .ok_or("codex_user_config_invalid")?;
                Selector::Path(fs::canonicalize(&path).unwrap_or(path))
            }
            (None, Some(name)) if !name.trim().is_empty() => Selector::Name(name.trim().into()),
            // Native ignores both/neither/empty-name selectors; never convert
            // an invalid combination into a valid disabling path selector.
            _ => continue,
        };
        rules.retain(|rule| rule.selector != selector);
        rules.push(Rule { selector, enabled: entry.enabled });
    }
    Ok(rules)
}

// Match fixed 0.163 AbsolutePathBuf ordering: home/platform expansion -> base
// join -> lexical ./.. elimination -> caller canonicalize. No filesystem walk
// is involved in eliminating a component, including a symlink or missing dir.
fn resolve_selector_path(path: &Path, base: &Path, home: &Path) -> Option<PathBuf> {
    if path.to_str().is_some_and(|value| value == "~" || value.starts_with("~/")
        || (cfg!(windows) && value.starts_with("~\\"))) && !home.is_absolute() { return None; }
    let expanded = match path.to_str() {
        Some("~") => home.to_path_buf(),
        Some(value) if value.starts_with("~/") => home.join(value[2..].trim_start_matches('/')),
        #[cfg(windows)]
        Some(value) if value.starts_with("~\\") => home.join(value[2..].trim_start_matches('\\')),
        _ => path.to_path_buf(),
    };
    #[cfg(windows)]
    let expanded = {
        let value = expanded.to_str()?;
        if let Some(rest) = value.strip_prefix(r"\\?\UNC\").or_else(|| value.strip_prefix(r"\\.\UNC\")) {
            PathBuf::from(format!(r"\\{rest}"))
        } else if let Some(rest) = value.strip_prefix(r"\\?\").or_else(|| value.strip_prefix(r"\\.\")) {
            let bytes = rest.as_bytes();
            if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
                && matches!(bytes[2], b'\\' | b'/') { PathBuf::from(rest) } else { expanded }
        } else { expanded }
    };
    if !base.is_absolute() { return None; }
    #[cfg(not(windows))]
    let joined = if expanded.is_absolute() { expanded } else { base.join(expanded) };
    #[cfg(windows)]
    let joined = {
        use std::path::Component;
        if expanded.is_absolute() || expanded.has_root() { base.join(expanded) }
        else {
            let mut parts = expanded.components();
            if let Some(Component::Prefix(prefix)) = parts.clone().next() {
                parts.next();
                let mut joined = PathBuf::from(prefix.as_os_str());
                if parts.clone().next().is_none() { joined.push(std::path::MAIN_SEPARATOR_STR); }
                else {
                    let skip = usize::from(matches!(base.components().next(), Some(Component::Prefix(_))));
                    for part in base.components().skip(skip).chain(parts) { joined.push(part.as_os_str()); }
                }
                joined
            } else { base.join(expanded) }
        }
    };
    let mut normalized = PathBuf::new();
    for part in joined.components() {
        match part {
            std::path::Component::CurDir => {},
            std::path::Component::ParentDir => { normalized.pop(); },
            _ => normalized.push(part.as_os_str()),
        }
    }
    normalized.is_absolute().then_some(normalized)
}

fn user_config(user: &Path, home: &Path, before_open: impl FnOnce()) -> Result<UserConfig, &'static str> {
    let path = user.join("config.toml");
    let before = match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Err("codex_user_config_unavailable"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
            && fs::symlink_metadata(&path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {
                return Ok(UserConfig { config: toml::Value::Table(Default::default()), rules: Vec::new() });
            }
        // No raw TOML or filesystem error text enters diagnostics.
        Err(_) => return Err("codex_user_config_unavailable"),
    };
    let raw = read_regular_file_bytes(&path, before_open).map_err(|_| "codex_user_config_unavailable")?;
    let current = fs::metadata(&path).map_err(|_| "codex_user_config_unavailable")?;
    if !same_regular_file(&before, &raw.opened) || !same_regular_file(&raw.opened, &raw.finished)
        || !same_regular_file(&raw.finished, &current) { return Err("codex_user_config_unavailable"); }
    let text = std::str::from_utf8(&raw.bytes).map_err(|_| "codex_user_config_invalid")?;
    let config: toml::Value = toml::from_str(text).map_err(|_| "codex_user_config_invalid")?;
    let rules = parse_rules(&config, user, home)?;
    Ok(UserConfig { config, rules })
}

pub(super) fn estimate(
    context: &DiscoveryContext,
    user: &Path,
    scan_profile: scanner::ScanProfile,
    metadata_profile: MetadataProfile,
    select_roots: impl FnOnce(Vec<SkillRoot>) -> Vec<SkillRoot>,
) -> AgentContextEstimate {
    estimate_before_open(context, user, scan_profile, metadata_profile, || {}, select_roots)
}

// A deterministic fixture hook in the real estimate chain, after pathname
// metadata but before the nonblocking open. Production always passes no-op.
fn estimate_before_open(
    context: &DiscoveryContext,
    user: &Path,
    scan_profile: scanner::ScanProfile,
    metadata_profile: MetadataProfile,
    before_open: impl FnOnce(),
    select_roots: impl FnOnce(Vec<SkillRoot>) -> Vec<SkillRoot>,
) -> AgentContextEstimate {
    let snapshot = match user_config(user, &context.home, before_open) {
        Ok(snapshot) => snapshot,
        Err(code) => {
            let mut estimate = estimate_skills(&CodexAdapter, Vec::new());
            estimate.diagnostic = Some(SkillDiscoveryDiagnostic {
                code: code.into(),
                message: "Codex User 技能配置无法读取或解析；启停状态与技能数量未知".into(),
                metadata_only: Vec::new(),
            });
            return estimate;
        }
    };
    // Config is read once, before roots/plugin enumeration. Both roots and
    // enablement consume this same bounded, parsed snapshot.
    let roots = select_roots(roots_from_config(context, user, &snapshot.config));
    let mut skills = discover(&RootSnapshot { roots }, context, scan_profile, metadata_profile);
    for skill in &mut skills {
        let path = fs::canonicalize(&skill.path).unwrap_or_else(|_| skill.path.clone());
        let mut enabled = true;
        for rule in &snapshot.rules {
            let matches = match &rule.selector {
                Selector::Path(selector) => *selector == path,
                Selector::Name(selector) => *selector == skill.name,
            };
            if matches { enabled = rule.enabled; }
        }
        if !enabled {
            skill.visibility = SkillVisibility::Disabled;
            skill.user_invocable = false;
        }
    }
    estimate_skills(&CodexAdapter, skills)
}

// Reuse discover/canonical identity/KeepAll and the existing Codex renderer.
// This snapshot only supplies explicit roots; it adds no selection algorithm.
struct RootSnapshot { roots: Vec<SkillRoot> }

impl AgentSkillPolicy for RootSnapshot {
    fn agent(&self) -> AgentKind { AgentKind::Codex }
    fn roots(&self, _context: &DiscoveryContext) -> Vec<SkillRoot> { self.roots.clone() }
    fn is_enabled(&self, dir: &Path, metadata: &SkillMetadata, context: &DiscoveryContext) -> bool {
        CodexAdapter.is_enabled(dir, metadata, context)
    }
    fn name_collision(&self) -> NameCollision { CodexAdapter.name_collision() }
    fn visibility(&self, dir: &Path, metadata: &SkillMetadata, context: &DiscoveryContext) -> SkillVisibility {
        CodexAdapter.visibility(dir, metadata, context)
    }
    fn prompt_path_for_entry(&self, root: &SkillRoot, dir: &Path, file: &Path,
        metadata: &SkillMetadata, context: &DiscoveryContext) -> Option<String> {
        CodexAdapter.prompt_path_for_entry(root, dir, file, metadata, context)
    }
    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        CodexAdapter.render_visible_metadata(skills)
    }
    fn render_initial_catalog(&self, skills: &[EffectiveSkill]) -> catalog::CatalogRender {
        CodexAdapter.render_initial_catalog(skills)
    }
    fn estimate_tokens(&self, rendered: &str) -> usize { CodexAdapter.estimate_tokens(rendered) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_read::REGULAR_CONFIG_LIMIT;
    use tempfile::{tempdir, TempDir};

    struct Fixture {
        temp: TempDir,
        context: DiscoveryContext,
        user: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempdir().unwrap();
            let repo = temp.path().join("repo");
            let cwd = repo.join("middle/leaf");
            let home = temp.path().join("home");
            let user = home.join("private-codex");
            fs::create_dir_all(&cwd).unwrap();
            fs::create_dir_all(&user).unwrap();
            Self { temp, context: DiscoveryContext { cwd, home, repository_root: Some(repo) }, user }
        }

        fn skill(&self, root: &Path, dir: &str, name: &str) -> PathBuf {
            let file = root.join(dir).join("SKILL.md");
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, format!("---\nname: {name}\ndescription: Fixture\n---\n")).unwrap();
            file
        }

        fn config(&self, config: &str) {
            fs::write(self.user.join("config.toml"), config).unwrap();
        }

        fn estimate(&self) -> AgentContextEstimate {
            // Explicit roots and User path: never inherit CODEX_HOME, inspect
            // real HOME/config, scan /etc, mutate env or start a subprocess.
            estimate(&self.context, &self.user,
                scanner::ScanProfile::Recursive {
                    max_depth: 6, max_directories: 2_000, max_entries: 20_000,
                }, MetadataProfile::StrictFrontmatter, |roots| roots.into_iter()
                    .filter(|root| root.path.starts_with(self.temp.path())).collect())
        }
    }

    fn toggle(path: &Path, enabled: bool) -> String {
        format!("[[skills.config]]\npath = {}\nenabled = {enabled}\n",
            toml::Value::String(path.to_string_lossy().into_owned()))
    }

    fn assert_config_unknown(estimate: &AgentContextEstimate, code: &str) {
        assert!(estimate.skills.is_empty());
        assert_eq!(estimate.diagnostic.as_ref().unwrap().code, code);
        let json = serde_json::to_value(estimate).unwrap();
        for field in ["discovered_count", "model_visible_count", "manual_only_count",
            "name_only_count", "conditional_count", "estimated_tokens"] {
            assert!(json[field].is_null(), "{field}");
        }
        assert!(!json.to_string().contains("synthetic-private-marker"));
    }

    #[cfg(unix)]
    fn fixture_fifo(path: &Path) {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // Fixture-only OS primitive; no command, child or native Agent.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }

    fn estimate_replacing_at_open(f: &Fixture, replace: impl FnOnce()) -> AgentContextEstimate {
        estimate_before_open(&f.context, &f.user,
            scanner::ScanProfile::Recursive { max_depth: 6, max_directories: 2_000, max_entries: 20_000 },
            MetadataProfile::StrictFrontmatter, replace, |roots| roots.into_iter()
                .filter(|root| root.path.starts_with(f.temp.path())).collect())
    }

    #[test]
    fn codex_scan_estimate_config_byte_boundary_and_oversize_are_bounded() {
        let f = Fixture::new();
        f.skill(&f.user.join("skills"), "demo", "demo");
        let mut exact = vec![b'#'; REGULAR_CONFIG_LIMIT as usize];
        *exact.last_mut().unwrap() = b'\n';
        fs::write(f.user.join("config.toml"), &exact).unwrap();
        assert_eq!(f.estimate().model_visible_count, 1);
        exact.push(b' ');
        fs::write(f.user.join("config.toml"), exact).unwrap();
        assert_config_unknown(&f.estimate(), "codex_user_config_unavailable");
        assert!(roots(&f.context, &f.user).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn codex_scan_estimate_fifo_config_returns_unknown_before_roots() {
        let f = Fixture::new();
        fixture_fifo(&f.user.join("config.toml"));
        assert_config_unknown(&f.estimate(), "codex_user_config_unavailable");
        assert!(roots(&f.context, &f.user).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn codex_scan_estimate_regular_to_fifo_open_window_returns_unknown() {
        let f = Fixture::new();
        f.config("# regular\n");
        let config = f.user.join("config.toml");
        let estimate = estimate_replacing_at_open(&f, || {
            fs::remove_file(&config).unwrap();
            fixture_fifo(&config);
        });
        assert_config_unknown(&estimate, "codex_user_config_unavailable");
    }

    #[cfg(unix)]
    #[test]
    fn codex_scan_estimate_regular_replacement_identity_is_unknown() {
        let f = Fixture::new();
        f.config("# original\n");
        let config = f.user.join("config.toml");
        let estimate = estimate_replacing_at_open(&f, || {
            fs::rename(&config, f.user.join("old-config.toml")).unwrap();
            fs::write(&config, "# synthetic-private-marker\n").unwrap();
        });
        assert_config_unknown(&estimate, "codex_user_config_unavailable");
    }

    #[cfg(unix)]
    #[test]
    fn codex_scan_estimate_legal_config_symlink_shares_rules_and_plugin_snapshot() {
        let f = Fixture::new();
        let plugin = f.user.join("plugins/cache/market/demo/1.0");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(plugin.join("plugin.json"),
            r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"Fixture plugin"}"#).unwrap();
        f.skill(&plugin.join("skills"), "from-plugin", "from-plugin");
        let target = f.temp.path().join("linked-config.toml");
        fs::write(&target, "[plugins.'demo@market']\nenabled = true\n\
            [[skills.config]]\nname = 'from-plugin'\nenabled = false\n").unwrap();
        std::os::unix::fs::symlink(&target, f.user.join("config.toml")).unwrap();
        let estimate = estimate(&f.context, &f.user,
            scanner::ScanProfile::Recursive { max_depth: 6, max_directories: 2_000, max_entries: 20_000 },
            MetadataProfile::StrictFrontmatter, |roots| {
                // After root selection, replacing config cannot change rules.
                fs::write(&target, "[plugins.'demo@market']\nenabled = false\n").unwrap();
                roots.into_iter().filter(|root| root.path.starts_with(f.temp.path())).collect()
            });
        assert!(estimate.diagnostic.is_none());
        assert_eq!(estimate.skills.len(), 1);
        assert!(estimate.skills[0].is_plugin());
        assert_eq!(estimate.skills[0].visibility, SkillVisibility::Disabled);
        assert!(!estimate.skills[0].user_invocable);
    }

    #[test]
    fn codex_scan_estimate_raw_rule_budget_precedes_dedup_and_selector_validation() {
        let f = Fixture::new();
        f.skill(&f.user.join("skills"), "demo", "demo");
        let repeated = "[[skills.config]]\nname = 'demo'\nenabled = false\n";
        f.config(&repeated.repeat(MAX_RULES));
        assert_eq!(f.estimate().skills[0].visibility, SkillVisibility::Disabled);
        f.config(&repeated.repeat(MAX_RULES + 1));
        assert_config_unknown(&f.estimate(), "codex_user_config_rule_limit");
        // Invalid selectors still consume the bounded raw-entry budget.
        f.config(&"[[skills.config]]\nenabled = false\n".repeat(MAX_RULES + 1));
        assert_config_unknown(&f.estimate(), "codex_user_config_rule_limit");
    }

    #[test]
    fn codex_scan_estimate_missing_parent_dotdot_is_lexical_before_canonicalize() {
        let f = Fixture::new();
        f.skill(&f.user.join("skills"), "demo", "demo");
        let missing = f.user.join("missing");
        assert!(!missing.exists());
        f.config("[[skills.config]]\npath = 'missing/../skills/demo/SKILL.md'\nenabled = false\n");
        assert_eq!(f.estimate().skills[0].visibility, SkillVisibility::Disabled);
        f.config("[[skills.config]]\npath = 'missing/../skills/demo/SKILL.md'\nenabled = false\n\
            [[skills.config]]\npath = './skills/demo/SKILL.md'\nenabled = true\n");
        assert_eq!(f.estimate().skills[0].visibility, SkillVisibility::Automatic);
        assert!(!missing.exists());
    }

    #[cfg(unix)]
    #[test]
    fn codex_scan_estimate_symlink_dotdot_disables_lexical_source_only() {
        let f = Fixture::new();
        let expected = f.skill(&f.user.join("skills"), "demo", "same");
        let other = f.skill(&f.context.home.join(".agents/skills"), "demo", "same");
        let link_target = f.context.home.join(".agents/deep");
        fs::create_dir_all(&link_target).unwrap();
        std::os::unix::fs::symlink(&link_target, f.user.join("link")).unwrap();
        f.config("[[skills.config]]\npath = 'link/../skills/demo/SKILL.md'\nenabled = false\n");
        let estimate = f.estimate();
        assert_eq!(estimate.skills.len(), 2);
        assert_eq!(estimate.skills.iter().find(|s| s.path == expected).unwrap().visibility, SkillVisibility::Disabled);
        assert_eq!(estimate.skills.iter().find(|s| s.path == other).unwrap().visibility, SkillVisibility::Automatic);
    }

    #[test]
    fn codex_scan_estimate_tilde_repeated_separator_stays_under_home() {
        let f = Fixture::new();
        f.skill(&f.context.home.join(".agents/skills"), "demo", "demo");
        f.config("[[skills.config]]\npath = '~//.agents/skills/demo/SKILL.md'\nenabled = false\n");
        assert_eq!(f.estimate().skills[0].visibility, SkillVisibility::Disabled);
        #[cfg(unix)]
        assert_eq!(resolve_selector_path(Path::new("../../child"), Path::new("/"), &f.context.home), Some(PathBuf::from("/child")));
    }

    #[test]
    fn codex_scan_unresolvable_selector_base_is_unknown() {
        let f = Fixture::new();
        assert!(resolve_selector_path(Path::new("skills/demo/SKILL.md"), Path::new("relative-base"), &f.context.home).is_none());
        assert!(resolve_selector_path(Path::new("~//skills/demo/SKILL.md"), &f.user, Path::new("relative-home")).is_none());
    }

    #[test]
    fn codex_scan_includes_middle_project_layer_without_config() {
        let f = Fixture::new();
        let repo = f.context.repository_root.as_ref().unwrap();
        f.skill(&repo.join(".codex/skills"), "outer", "outer");
        let middle = f.skill(&repo.join("middle/.codex/skills"), "middle", "middle");
        f.skill(&f.context.cwd.join(".codex/skills"), "leaf", "leaf");
        f.skill(&f.temp.path().join(".codex/skills"), "outside", "outside");
        let estimate = f.estimate();
        assert_eq!(estimate.discovered_count, 3);
        assert!(estimate.skills.iter().any(|s| s.path == middle && s.scope == SkillScope::Repository));
        assert!(!repo.join("middle/.codex/config.toml").exists());
    }

    #[test]
    fn codex_scan_without_repository_boundary_stays_at_cwd() {
        let mut f = Fixture::new();
        f.context.repository_root = None;
        f.skill(&f.context.cwd.parent().unwrap().join(".codex/skills"), "parent", "parent");
        f.skill(&f.context.cwd.parent().unwrap().join(".agents/skills"), "parent", "parent");
        f.skill(&f.context.cwd.join(".agents/skills"), "local", "local");
        let estimate = f.estimate();
        assert_eq!(estimate.skills.len(), 1);
        assert_eq!(estimate.skills[0].name, "local");
    }

    #[test]
    fn codex_scan_keeps_same_name_distinct_sources_enabled() {
        let f = Fixture::new();
        let repo = f.skill(&f.context.cwd.join(".codex/skills"), "repo", "same");
        let user = f.skill(&f.user.join("skills"), "user", "same");
        let estimate = f.estimate();
        assert_eq!(estimate.discovered_count, 2);
        assert_eq!(estimate.model_visible_count, 2);
        assert!(estimate.skills.iter().all(|s| s.visibility == SkillVisibility::Automatic && s.user_invocable));
        assert_ne!(fs::canonicalize(repo).unwrap(), fs::canonicalize(user).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn codex_scan_canonical_file_alias_deduplicates_and_matches_path_rule() {
        let f = Fixture::new();
        let file = f.skill(&f.user.join("skills"), "original", "same");
        let alias = f.context.cwd.join(".codex/skills/alias/SKILL.md");
        fs::create_dir_all(alias.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&file, &alias).unwrap();
        f.config(&toggle(&file, false));
        let estimate = f.estimate();
        assert_eq!(estimate.skills.len(), 1);
        assert_eq!(estimate.skills[0].scope, SkillScope::Repository);
        assert_eq!(estimate.skills[0].visibility, SkillVisibility::Disabled);
        assert!(!estimate.skills[0].user_invocable);
        assert_eq!(estimate.model_visible_count, 0);
        // A later alias rule addresses the same canonical document.
        f.config(&(toggle(&file, false) + &toggle(&alias, true)));
        assert_eq!(f.estimate().skills[0].visibility, SkillVisibility::Automatic);
    }

    #[cfg(unix)]
    #[test]
    fn codex_scan_directory_alias_keeps_nearest_project_provenance() {
        let f = Fixture::new();
        let root = f.context.repository_root.as_ref().unwrap().join(".codex/skills");
        let file = f.skill(&root, "original", "same");
        let nearest = f.context.cwd.join(".codex/skills");
        fs::create_dir_all(&nearest).unwrap();
        std::os::unix::fs::symlink(file.parent().unwrap(), nearest.join("alias")).unwrap();
        let estimate = f.estimate();
        assert_eq!(estimate.skills.len(), 1);
        assert_eq!(estimate.skills[0].root_path.as_ref(), Some(&nearest));
    }

    #[test]
    fn codex_scan_later_path_and_name_rules_override_in_order() {
        let f = Fixture::new();
        let first = f.skill(&f.context.cwd.join(".codex/skills"), "one", "same");
        let second = f.skill(&f.user.join("skills"), "two", "same");
        f.config(&("[[skills.config]]\nname = ' same '\nenabled = false\n".to_string()
            + &toggle(&first, true)));
        let estimate = f.estimate();
        assert_eq!(estimate.discovered_count, 2);
        assert_eq!(estimate.model_visible_count, 1);
        assert_eq!(estimate.skills.iter().find(|s| s.path == second).unwrap().visibility, SkillVisibility::Disabled);
        f.config(&(toggle(&first, false) + "[[skills.config]]\nname = 'same'\nenabled = true\n"));
        assert!(f.estimate().skills.iter().all(|s| s.visibility == SkillVisibility::Automatic));
        f.config(&(toggle(&first, false) + &toggle(&first, true)));
        assert_eq!(f.estimate().model_visible_count, 2);
        // The last repeated selector must retain its new position relative to name rules.
        f.config(&(toggle(&first, true) + "[[skills.config]]\nname = 'same'\nenabled = false\n"
            + &toggle(&first, true)));
        assert_eq!(f.estimate().model_visible_count, 1);
    }

    #[test]
    fn codex_scan_ignores_invalid_selector_combinations() {
        let f = Fixture::new();
        let file = f.skill(&f.user.join("skills"), "demo", "demo");
        f.config(&format!("[[skills.config]]\npath = {}\nname = 'demo'\nenabled = false\n\
            [[skills.config]]\nenabled = false\n\
            [[skills.config]]\nname = '  '\nenabled = false\n",
            toml::Value::String(file.to_string_lossy().into_owned())));
        let estimate = f.estimate();
        assert!(estimate.diagnostic.is_none());
        assert_eq!(estimate.model_visible_count, 1);
    }

    #[test]
    fn codex_scan_directory_selector_does_not_disable_document() {
        let f = Fixture::new();
        let file = f.skill(&f.user.join("skills"), "demo", "demo");
        f.config(&toggle(file.parent().unwrap(), false));
        assert_eq!(f.estimate().model_visible_count, 1);
    }

    #[test]
    fn codex_scan_only_user_rules_apply_to_all_skill_scopes() {
        let f = Fixture::new();
        let repo = f.context.repository_root.as_ref().unwrap();
        f.skill(&repo.join("middle/.codex/skills"), "project", "same");
        f.skill(&f.user.join("skills"), "user", "same");
        f.skill(&f.user.join("skills/.system"), "system", "same");
        let project_config = repo.join("middle/.codex/config.toml");
        fs::write(&project_config, "[[skills.config]]\nname = 'same'\nenabled = false\n").unwrap();
        // A System root's own config is not a User config layer either.
        fs::write(f.user.join("skills/.system/config.toml"), "[[skills.config]]\nname = 'same'\nenabled = false\n").unwrap();
        let estimate = f.estimate();
        assert_eq!(estimate.discovered_count, 3);
        assert_eq!(estimate.model_visible_count, 3);
        assert!(estimate.skills.iter().any(|s| s.scope == SkillScope::System));
        f.config("[[skills.config]]\nname = 'same'\nenabled = false\n");
        let estimate = f.estimate();
        assert_eq!(estimate.discovered_count, 3);
        assert_eq!(estimate.model_visible_count, 0);
        assert!(estimate.skills.iter().all(|s| s.visibility == SkillVisibility::Disabled));
        // User later true wins even while Project still says false.
        f.config("[[skills.config]]\nname = 'same'\nenabled = false\n[[skills.config]]\nname = 'same'\nenabled = true\n");
        assert_eq!(f.estimate().model_visible_count, 3);
        fs::write(project_config, "[invalid Project TOML").unwrap();
        assert!(f.estimate().diagnostic.is_none());
    }

    #[test]
    fn codex_scan_config_errors_preserve_unknown_counts_and_redact_content() {
        let f = Fixture::new();
        f.skill(&f.user.join("skills"), "demo", "demo");
        for invalid in ["[synthetic-private-marker", "skills = 42", "[skills]\nconfig = false",
            "[[skills.config]]\nname = 'demo'\nenabled = 'synthetic-private-marker'",
            "[[skills.config]]\nname = 'demo'", "[[skills.config]]\nname = 1\nenabled = false",
            "[[skills.config]]\npath = 1\nenabled = false"] {
            f.config(invalid);
            let estimate = f.estimate();
            let json = serde_json::to_value(&estimate).unwrap();
            assert!(estimate.diagnostic.is_some());
            for field in ["discovered_count", "model_visible_count", "manual_only_count",
                "name_only_count", "conditional_count", "estimated_tokens"] {
                assert!(json[field].is_null(), "{field}");
            }
            assert!(!json.to_string().contains("synthetic-private-marker"));
            assert!(estimate.skills.is_empty());
        }
    }

    #[test]
    fn codex_scan_unreadable_user_config_is_unknown_not_all_enabled() {
        let f = Fixture::new();
        fs::create_dir(f.user.join("config.toml")).unwrap();
        let estimate = f.estimate();
        assert_eq!(estimate.diagnostic.unwrap().code, "codex_user_config_unavailable");
    }

    #[test]
    fn codex_scan_missing_user_config_is_known_empty_rules() {
        let f = Fixture::new();
        f.skill(&f.user.join("skills"), "demo", "demo");
        let estimate = f.estimate();
        assert!(estimate.diagnostic.is_none());
        assert_eq!(estimate.model_visible_count, 1);
    }

    #[test]
    fn codex_scan_relative_path_is_user_layer_relative() {
        let f = Fixture::new();
        f.skill(&f.user.join("skills"), "demo", "demo");
        f.config("[[skills.config]]\npath = 'skills/demo/SKILL.md'\nenabled = false\n");
        assert_eq!(f.estimate().skills[0].visibility, SkillVisibility::Disabled);
    }

    #[test]
    fn codex_scan_home_path_expansion_uses_fixture_home() {
        let f = Fixture::new();
        f.skill(&f.context.home.join(".agents/skills"), "demo", "demo");
        f.config("[[skills.config]]\npath = '~/.agents/skills/demo/SKILL.md'\nenabled = false\n");
        assert_eq!(f.estimate().skills[0].visibility, SkillVisibility::Disabled);
    }

    #[test]
    fn codex_scan_enabled_path_does_not_add_external_root() {
        let f = Fixture::new();
        let file = f.skill(&f.temp.path().join("external"), "demo", "demo");
        f.config(&toggle(&file, true));
        assert!(f.estimate().skills.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn codex_scan_manual_policy_uses_canonical_source_and_disabled_takes_precedence() {
        let f = Fixture::new();
        let file = f.skill(&f.user.join("skills"), "demo", "demo");
        let sidecar = file.parent().unwrap().join("agents");
        fs::create_dir_all(&sidecar).unwrap();
        fs::write(sidecar.join("openai.yaml"), "policy:\n  allow_implicit_invocation: false\n").unwrap();
        let alias = f.context.cwd.join(".codex/skills/alias/SKILL.md");
        fs::create_dir_all(alias.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&file, &alias).unwrap();
        let estimate = f.estimate();
        assert_eq!(estimate.skills.len(), 1);
        assert_eq!(estimate.skills[0].visibility, SkillVisibility::ManualOnly);
        assert!(estimate.skills[0].user_invocable);
        assert_eq!(estimate.manual_only_count, 1);
        f.config(&toggle(&alias, false));
        let estimate = f.estimate();
        assert_eq!(estimate.skills[0].visibility, SkillVisibility::Disabled);
        assert!(!estimate.skills[0].user_invocable);
        assert_eq!(estimate.manual_only_count, 0);
        f.config(&(toggle(&alias, false) + &toggle(&file, true)));
        assert_eq!(f.estimate().skills[0].visibility, SkillVisibility::ManualOnly);
    }

    #[cfg(unix)]
    #[test]
    fn codex_scan_dangling_config_symlink_preserves_unknown() {
        let f = Fixture::new();
        std::os::unix::fs::symlink(f.user.join("missing.toml"), f.user.join("config.toml")).unwrap();
        let estimate = f.estimate();
        assert_eq!(estimate.diagnostic.unwrap().code, "codex_user_config_unavailable");
    }

    #[test]
    fn codex_scan_toml_decodes_windows_path_strings_and_key_order() {
        // String decoding only: no Windows paths are resolved on this host.
        let config: SkillsConfig = toml::from_str(r#"
[[config]]
enabled = false
path = "C:\\Users\\test\\skills\\demo\\SKILL.md"
[[config]]
path = 'D:\skills\active\SKILL.md'
enabled = true
"#).unwrap();
        assert_eq!(config.config[0].path.as_deref(), Some(Path::new(r"C:\Users\test\skills\demo\SKILL.md")));
        assert!(!config.config[0].enabled);
        assert_eq!(config.config[1].path.as_deref(), Some(Path::new(r"D:\skills\active\SKILL.md")));
        assert!(config.config[1].enabled);
    }
}
