//! Agent-specific discovery and context-cost estimation for effective skills.
//!
//! The public interface returns an initial-context snapshot. Agent adapters own
//! static discovery profiles; scanner and catalog modules are internal primitives.

use crate::config_read::{read_regular_file_bytes, RegularFileReadError};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

mod adapters;
mod additional_agents;
mod catalog;
mod codex;
mod copilot_origin;
mod scanner;
mod desktop_agents;
mod hermes;
mod grok_metadata;
mod grok_selection;
mod grok_state;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Codex,
    ClaudeCode,
    Cursor,
    OpenCode,
    Copilot,
    Antigravity,
    Amp,
    Droid,
    Pi,
    Proma,
    Omp,
    DimAgent,
    Grok,
    OpenClaw,
    Hermes,
    Trae,
    WorkBuddy,
}

impl AgentKind {
    pub fn from_id(id: &str) -> Option<Self> {
        [Self::Codex, Self::ClaudeCode, Self::Cursor, Self::OpenCode, Self::Copilot,
            Self::Antigravity, Self::Amp, Self::Droid, Self::Pi, Self::Proma,
            Self::Omp, Self::DimAgent, Self::Grok, Self::OpenClaw, Self::Hermes,
            Self::Trae, Self::WorkBuddy]
            .into_iter().find(|agent| agent.id() == id)
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
            Self::Copilot => "copilot",
            Self::Antigravity => "antigravity",
            Self::Amp => "amp",
            Self::Droid => "droid",
            Self::Pi => "pi",
            Self::Proma => "proma",
            Self::Omp => "omp",
            Self::DimAgent => "dimagent",
            Self::Grok => "grok",
            Self::OpenClaw => "openclaw",
            Self::Hermes => "hermes",
            Self::Trae => "trae",
            Self::WorkBuddy => "workbuddy",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
            Self::OpenCode => "OpenCode",
            Self::Copilot => "GitHub Copilot",
            Self::Antigravity => "Antigravity",
            Self::Amp => "Amp",
            Self::Droid => "Droid",
            Self::Pi => "Pi",
            Self::Proma => "Proma",
            Self::Omp => "omp",
            Self::DimAgent => "DimAgent",
            Self::Grok => "Grok",
            Self::OpenClaw => "OpenClaw",
            Self::Hermes => "Hermes",
            Self::Trae => "Trae",
            Self::WorkBuddy => "WorkBuddy",
        }
    }

    pub fn is_global_only(self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillVisibility {
    Automatic,
    NameOnly,
    ManualOnly,
    Conditional,
    /// Discovered, but native selection between distinct same-name sources is unknown.
    Unverified,
    /// Retained for source inspection, but superseded in the native catalog.
    Shadowed,
    /// Still listed by the host, but excluded from its model and manual invocation.
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillScope {
    Local,
    Repository,
    User,
    System,
}

/// Provenance for an effective Skill or a discovery root.
///
/// This is intentionally separate from [`SkillScope`]. A plugin can be
/// installed in a user or system scope while still needing its own UI group.
///
/// Plugin identity is the stable provider identity (for example
/// `deploy@official`), while `display_name` is the human-facing name read
/// from the provider manifest or derived from that identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SkillSource {
    Filesystem,
    Builtin,
    Plugin { id: String, display_name: String },
}

impl SkillSource {
    pub fn is_plugin(&self) -> bool {
        matches!(self, Self::Plugin { .. })
    }

    pub fn plugin_id(&self) -> Option<&str> {
        match self {
            Self::Plugin { id, .. } => Some(id),
            _ => None,
        }
    }

    pub fn plugin_display_name(&self) -> Option<&str> {
        match self {
            Self::Plugin { display_name, .. } => Some(display_name),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct EffectiveSkill {
    /// Provider registration ID. Most providers use the frontmatter name;
    /// OpenCode V2 derives it from the source path.
    pub id: String,
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub path: PathBuf,
    /// Discovery root that made this Skill effective. The Projects UI uses
    /// this exact value for provenance instead of trying to reconstruct a
    /// provider-specific user/project directory from the Skill file path.
    pub root_path: Option<PathBuf>,
    /// The path string, if this provider actually places one in its model
    /// catalog. This is intentionally not inferred from [`Self::path`].
    pub prompt_path: Option<String>,
    pub scope: SkillScope,
    pub visibility: SkillVisibility,
    pub user_invocable: bool,
    pub source: SkillSource,
}

impl EffectiveSkill {
    pub fn is_plugin(&self) -> bool {
        self.source.is_plugin()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct NativeSkillMetadata {
    pub name: String,
    pub description: String,
    pub source_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invocation_name: Option<String>,
    pub user_invocable: bool,
    pub disabled: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SkillDiscoveryDiagnostic {
    pub code: String,
    pub message: String,
    /// Safe native display metadata without a verified effective source. These
    /// are never filesystem Skills, installation targets, or numeric counts.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub metadata_only: Vec<NativeSkillMetadata>,
}

#[derive(Clone, Debug)]
pub struct AgentContextEstimate {
    pub agent: AgentKind,
    pub discovered_count: usize,
    pub model_visible_count: usize,
    pub manual_only_count: usize,
    pub name_only_count: usize,
    pub conditional_count: usize,
    pub estimated_tokens: usize,
    pub skills: Vec<EffectiveSkill>,
    /// Counts are not an observation when this is Some. Render unknown, not 0.
    pub diagnostic: Option<SkillDiscoveryDiagnostic>,
}

impl Serialize for AgentContextEstimate {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("AgentContextEstimate", 8 + usize::from(self.diagnostic.is_some()))?;
        state.serialize_field("agent", &self.agent)?;
        let known = self.diagnostic.is_none();
        macro_rules! count { ($field:ident) => { state.serialize_field(stringify!($field), &known.then_some(self.$field))?; }; }
        count!(discovered_count);
        count!(model_visible_count);
        count!(manual_only_count);
        count!(name_only_count);
        count!(conditional_count);
        count!(estimated_tokens);
        state.serialize_field("skills", &self.skills)?;
        if let Some(diagnostic) = &self.diagnostic { state.serialize_field("diagnostic", diagnostic)?; }
        state.end()
    }
}

impl AgentContextEstimate {
    /// Plugin contributions are kept in the same effective set as filesystem
    /// Skills, but this view lets the UI render a plugin-specific tab without
    /// reimplementing source classification.
    pub fn plugin_skills(&self) -> impl Iterator<Item = &EffectiveSkill> {
        self.skills.iter().filter(|skill| skill.is_plugin())
    }
}

pub struct EffectiveSkillGroup<'a> {
    entries: Vec<(AgentKind, &'a EffectiveSkill)>,
}

impl<'a> EffectiveSkillGroup<'a> {
    pub fn name(&self) -> &str {
        &self.entries[0].1.name
    }

    pub fn entries(&self) -> &[(AgentKind, &'a EffectiveSkill)] {
        &self.entries
    }
}

/// Groups the same logical Skill across Agent snapshots.
///
/// Agents can expose different names for one filesystem entry (for example,
/// Claude's `build_from_zero` and Grok's normalized `build-from-zero`). A
/// canonical filesystem path joins entries into one group. Names alone do
/// not establish identity; built-ins and plugins also retain their provider.
pub fn group_effective_skills<'a>(
    entries: impl IntoIterator<Item = (AgentKind, &'a EffectiveSkill)>,
) -> Vec<EffectiveSkillGroup<'a>> {
    group_effective_skills_with_identity(entries).into_iter().map(|(_, group)| group).collect()
}

// Preserve the exact canonical key used for grouping. A fallback path is still
// useful for display, but cannot establish native source identity for Copilot.
fn group_effective_skills_with_identity<'a>(
    entries: impl IntoIterator<Item = (AgentKind, &'a EffectiveSkill)>,
) -> Vec<(Option<PathBuf>, EffectiveSkillGroup<'a>)> {
    let mut groups = BTreeMap::<_, (bool, Vec<_>)>::new();
    for (agent, skill) in entries {
        let (provider, id) = match &skill.source {
            SkillSource::Filesystem => (String::new(), String::new()),
            SkillSource::Builtin => (format!("builtin:{}", agent.id()), skill.id.clone()),
            SkillSource::Plugin { id, .. } => (format!("plugin:{}", agent.id()), id.clone()),
        };
        let canonical = skill.path.canonicalize();
        let known = canonical.is_ok();
        let path = canonical.unwrap_or_else(|_| skill.path.clone());
        let group = groups.entry((provider, id, path)).or_insert_with(|| (true, Vec::new()));
        group.0 &= known;
        group.1.push((agent, skill));
    }

    let mut result = groups
        .into_iter()
        .map(|((_, _, path), (known, entries))| (known.then_some(path), EffectiveSkillGroup { entries }))
        .collect::<Vec<_>>();
    result.sort_by(|left, right| left.1.name().cmp(right.1.name()));
    result
}

#[derive(Clone, Debug)]
struct DiscoveryContext {
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub repository_root: Option<PathBuf>,
}

#[derive(Clone, Debug)]
enum OpenClawRootBoundary { SkillOnly, Contained, TrustedTargets }

#[derive(Clone, Debug)]
struct SkillRoot {
    pub path: PathBuf,
    pub scope: SkillScope,
    pub include_root_markdown: bool,
    pub flat_markdown_only: bool,
    pub direct_children_only: bool,
    pub follow_directory_symlinks: bool,
    pub exact_skill_file: Option<PathBuf>,
    pub source: SkillSource,
    openclaw_boundary: Option<OpenClawRootBoundary>,
}

impl SkillRoot {
    fn new(path: PathBuf, scope: SkillScope) -> Self {
        Self {
            path,
            scope,
            include_root_markdown: false,
            flat_markdown_only: false,
            direct_children_only: false,
            follow_directory_symlinks: true,
            exact_skill_file: None,
            source: SkillSource::Filesystem,
            openclaw_boundary: None,
        }
    }

    fn openclaw(mut self, boundary: OpenClawRootBoundary) -> Self {
        self.openclaw_boundary = Some(boundary);
        self
    }

    fn plugin(
        path: PathBuf,
        scope: SkillScope,
        id: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Self {
        let mut root = Self::new(path, scope);
        root.source = SkillSource::Plugin {
            id: id.into(),
            display_name: display_name.into(),
        };
        root
    }

    fn with_source(mut self, source: SkillSource) -> Self {
        self.source = source;
        self
    }

    fn with_root_markdown(mut self) -> Self {
        self.include_root_markdown = true;
        self
    }

    fn flat_markdown(mut self) -> Self {
        self.include_root_markdown = true;
        self.flat_markdown_only = true;
        self
    }

    fn direct_children(mut self) -> Self {
        self.direct_children_only = true;
        self
    }

    fn without_directory_symlinks(mut self) -> Self {
        self.follow_directory_symlinks = false;
        self
    }

    fn exact(skill_file: PathBuf, scope: SkillScope) -> Self {
        Self {
            path: skill_file
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            scope,
            include_root_markdown: true,
            flat_markdown_only: false,
            direct_children_only: false,
            follow_directory_symlinks: true,
            exact_skill_file: Some(skill_file),
            source: SkillSource::Filesystem,
            openclaw_boundary: None,
        }
    }
}

#[derive(Clone, Debug)]
struct SkillMetadata {
    pub name: String,
    pub description: String,
    pub has_explicit_description: bool,
    pub when_to_use: Option<String>,
    pub source_path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NameCollision {
    KeepAll,
    FirstWins,
    FirstRootWins,
    LastWins,
}

/// Internal entry rules composed by the Agent adapters.
trait AgentSkillPolicy {
    fn agent(&self) -> AgentKind;
    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot>;
    fn is_enabled(
        &self,
        _skill_dir: &Path,
        _metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> bool {
        true
    }
    fn effective_name(&self, _skill_dir: &Path, metadata: &SkillMetadata) -> String {
        metadata.name.clone()
    }
    fn effective_name_for_root(
        &self,
        root: &SkillRoot,
        skill_dir: &Path,
        metadata: &SkillMetadata,
    ) -> String {
        let _ = root;
        self.effective_name(skill_dir, metadata)
    }
    fn effective_id_for_root(
        &self,
        root: &SkillRoot,
        skill_dir: &Path,
        skill_file: &Path,
        metadata: &SkillMetadata,
    ) -> String {
        let _ = skill_file;
        self.effective_name_for_root(root, skill_dir, metadata)
    }
    fn prompt_path_for_entry(
        &self,
        root: &SkillRoot,
        skill_dir: &Path,
        skill_file: &Path,
        metadata: &SkillMetadata,
        context: &DiscoveryContext,
    ) -> Option<String> {
        let _ = (root, skill_dir, skill_file, metadata, context);
        None
    }
    fn is_enabled_for_root(
        &self,
        root: &SkillRoot,
        skill_dir: &Path,
        metadata: &SkillMetadata,
        context: &DiscoveryContext,
    ) -> bool {
        let _ = root;
        self.is_enabled(skill_dir, metadata, context)
    }
    fn is_enabled_for_entry(
        &self,
        root: &SkillRoot,
        skill_dir: &Path,
        skill_file: &Path,
        metadata: &SkillMetadata,
        context: &DiscoveryContext,
    ) -> bool {
        let _ = skill_file;
        self.is_enabled_for_root(root, skill_dir, metadata, context)
    }
    fn name_collision(&self) -> NameCollision;
    fn user_invocable(&self, _skill_dir: &Path, _metadata: &SkillMetadata, _context: &DiscoveryContext) -> bool {
        true
    }
    fn visibility(
        &self,
        skill_dir: &Path,
        metadata: &SkillMetadata,
        context: &DiscoveryContext,
    ) -> SkillVisibility;
    fn visibility_for_root(
        &self,
        root: &SkillRoot,
        skill_dir: &Path,
        metadata: &SkillMetadata,
        context: &DiscoveryContext,
    ) -> SkillVisibility {
        let _ = root;
        self.visibility(skill_dir, metadata, context)
    }
    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String;
    fn render_initial_catalog(&self, skills: &[EffectiveSkill]) -> catalog::CatalogRender {
        catalog::CatalogRender::all(self.render_visible_metadata(skills), skills.len())
    }
    fn estimate_tokens(&self, rendered: &str) -> usize;
}

struct CodexAdapter;
struct ClaudeCodeAdapter;
struct CursorPolicy;
struct OpenCodePolicy;
struct CopilotPolicy {
    roots: Vec<SkillRoot>,
}
struct AntigravityPolicy;
struct AmpPolicy;
struct DroidPolicy;
struct PiAdapter;
struct PromaAdapter;

fn proma_workspaces(context: &DiscoveryContext) -> Vec<crate::proma::Workspace> {
    let root = std::env::var_os("KITTER_PROMA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| context.home.join(".proma"));
    crate::proma::workspaces_for_project(&root, &context.cwd).unwrap_or_default()
}
struct GrokAdapter;
struct OpenClawPolicy;
struct HermesPolicy;

#[cfg(test)]
#[allow(non_upper_case_globals)]
const CodexPolicy: CodexAdapter = CodexAdapter;
#[cfg(test)]
#[allow(non_upper_case_globals)]
const ClaudeCodePolicy: ClaudeCodeAdapter = ClaudeCodeAdapter;
#[cfg(test)]
#[allow(non_upper_case_globals)]
const PiPolicy: PiAdapter = PiAdapter;
#[cfg(test)]
#[allow(non_upper_case_globals)]
const GrokPolicy: GrokAdapter = GrokAdapter;

pub fn estimate_project(project: &Path) -> Vec<AgentContextEstimate> {
    estimate_project_for_agents(project, &crate::agents::installed_agents())
}

pub fn estimate_project_for_agents(
    project: &Path,
    agents: &[crate::agents::AgentIconInfo],
) -> Vec<AgentContextEstimate> {
    let discovered = crate::project_discovery::discover_agent_projects().unwrap_or_default();
    estimate_project_with_catalog(project, agents, &discovered)
}

pub fn estimate_project_with_catalog(
    project: &Path,
    agents: &[crate::agents::AgentIconInfo],
    discovered: &[crate::project_discovery::ProjectEntry],
) -> Vec<AgentContextEstimate> {
    let home = dirs::home_dir().unwrap_or_else(|| project.to_path_buf());
    let associated = crate::project_discovery::agents_for_project(project, agents, discovered);
    estimate_for_agents(project, &home, &associated)
}

/// Explicit compatibility inspection, also used with isolated fixture homes.
pub fn estimate_project_with_home(project: &Path, home: &Path) -> Vec<AgentContextEstimate> {
    estimate_for_agents(project, home, crate::agents::AGENT_ICON_ORDER)
}

/// An explicit host inspection may select an execution cwd without creating a
/// native project association. Reuse the same scanner and host policy.
pub fn estimate_project_for_explicit_agent(project: &Path, agent: AgentKind) -> Vec<AgentContextEstimate> {
    let home = dirs::home_dir().unwrap_or_else(|| project.to_path_buf());
    let agents = crate::agents::AGENT_ICON_ORDER.iter().copied()
        .filter(|info| info.id == agent.id()).collect::<Vec<_>>();
    estimate_for_agents(project, &home, &agents)
}

fn estimate_for_agents(
    project: &Path,
    home: &Path,
    agents: &[crate::agents::AgentIconInfo],
) -> Vec<AgentContextEstimate> {
    let context = DiscoveryContext {
        cwd: project.to_path_buf(),
        home: home.to_path_buf(),
        repository_root: find_repository_root(project),
    };
    let mut estimates = adapters::inspect_project(&context, project == home, agents);
    // The legacy adapter table includes OpenClaw only in its global pass.
    // Project inspection uses the very same policy, with the selected cwd.
    if project != home && agents.iter().any(|agent| agent.id == "openclaw") {
        estimates.push(estimate_with_policy(&OpenClawPolicy, &context));
    }
    estimates
}

#[derive(Clone, Copy)]
enum MetadataProfile {
    StrictFrontmatter,
    BodyFallback,
    ProviderFrontmatter,
    WorkBuddy,
}

fn estimate_with_policy(
    policy: &dyn AgentSkillPolicy,
    context: &DiscoveryContext,
) -> AgentContextEstimate {
    if policy.agent() == AgentKind::Hermes { return hermes::estimate(context); }
    if policy.agent() == AgentKind::OpenClaw {
        return match openclaw_config(context) {
            Ok(snapshot) => estimate_with_profile(&OpenClawSnapshotPolicy { snapshot }, context,
                scanner::ScanProfile::Recursive { max_depth: usize::MAX, max_directories: usize::MAX, max_entries: usize::MAX },
                MetadataProfile::StrictFrontmatter),
            Err(failure) => {
                let mut estimate = estimate_skills(policy, Vec::new());
                estimate.diagnostic = Some(SkillDiscoveryDiagnostic {
                    code: failure.code().into(), message: failure.message().into(),
                    metadata_only: Vec::new(),
                });
                estimate
            }
        };
    }
    estimate_with_profile(
        policy,
        context,
        scanner::ScanProfile::Recursive {
            max_depth: usize::MAX,
            max_directories: usize::MAX,
            max_entries: usize::MAX,
        },
        MetadataProfile::StrictFrontmatter,
    )
}

fn estimate_with_profile(
    policy: &dyn AgentSkillPolicy,
    context: &DiscoveryContext,
    scan_profile: scanner::ScanProfile,
    metadata_profile: MetadataProfile,
) -> AgentContextEstimate {
    if policy.agent() == AgentKind::Codex {
        return codex::estimate(context, &codex_home(context), scan_profile, metadata_profile, |roots| roots);
    }
    let skills = discover(policy, context, scan_profile, metadata_profile);
    estimate_skills(policy, skills)
}

fn estimate_skills(
    policy: &dyn AgentSkillPolicy,
    skills: Vec<EffectiveSkill>,
) -> AgentContextEstimate {
    let visible = skills
        .iter()
        .filter(|skill| {
            matches!(
                skill.visibility,
                SkillVisibility::Automatic | SkillVisibility::NameOnly
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let rendered = policy.render_initial_catalog(&visible);
    AgentContextEstimate {
        agent: policy.agent(),
        discovered_count: skills.len(),
        model_visible_count: rendered.included_count,
        manual_only_count: skills
            .iter()
            .filter(|s| s.visibility == SkillVisibility::ManualOnly)
            .count(),
        name_only_count: skills
            .iter()
            .filter(|s| s.visibility == SkillVisibility::NameOnly)
            .count(),
        conditional_count: skills
            .iter()
            .filter(|s| s.visibility == SkillVisibility::Conditional)
            .count(),
        estimated_tokens: policy.estimate_tokens(&rendered.text),
        skills,
        diagnostic: None,
    }
}

pub fn is_manual_skill(skill_dir: &Path) -> bool {
    let Some(metadata) = read_metadata(&skill_dir.join("SKILL.md")) else {
        return false;
    };
    frontmatter_visibility(&metadata) == SkillVisibility::ManualOnly || codex_manual_only(skill_dir)
}

fn discover(
    policy: &dyn AgentSkillPolicy,
    context: &DiscoveryContext,
    scan_profile: scanner::ScanProfile,
    metadata_profile: MetadataProfile,
) -> Vec<EffectiveSkill> {
    let mut result = Vec::new();
    let mut ids = HashSet::new();
    let mut first_roots = HashMap::new();
    let mut canonical_paths = HashSet::new();
    for root in policy.roots(context) {
        for skill_file in scanner::scan(&root, scan_profile) {
            let skill_dir = skill_file.parent().unwrap_or(&root.path).to_path_buf();
            // Codex identifies canonical SKILL.md documents, including file
            // aliases. Other directory Skills use their directory; Pi and
            // explicit roots can contain independent root Markdown files.
            let canonical_target = if policy.agent() == AgentKind::Codex
                || root.exact_skill_file.is_some()
                || (root.include_root_markdown && skill_dir == root.path)
            {
                &skill_file
            } else {
                &skill_dir
            };
            let canonical = fs::canonicalize(canonical_target)
                .unwrap_or_else(|_| canonical_target.to_path_buf());
            if !canonical_paths.insert(canonical.clone()) {
                continue;
            }
            let Some(metadata) = read_metadata_with_profile(&skill_file, metadata_profile) else {
                continue;
            };
            if !policy.is_enabled_for_entry(&root, &skill_dir, &skill_file, &metadata, context) {
                if policy.agent() == AgentKind::OpenClaw { canonical_paths.remove(&canonical); }
                continue;
            }
            let name = policy.effective_name_for_root(&root, &skill_dir, &metadata);
            let id = policy.effective_id_for_root(&root, &skill_dir, &skill_file, &metadata);
            let mut shadowed = false;
            match policy.name_collision() {
                NameCollision::KeepAll => {}
                NameCollision::FirstWins => shadowed = !ids.insert(id.clone()),
                NameCollision::FirstRootWins => {
                    let root_path = crate::project_discovery::path_identity(&root.path);
                    shadowed = first_roots.entry(id.clone()).or_insert_with(|| root_path.clone()) != &root_path;
                }
                NameCollision::LastWins => {
                    if ids.insert(id.clone()) {
                        // First occurrence; nothing to replace.
                    } else if let Some(index) = result
                        .iter()
                        .position(|skill: &EffectiveSkill| skill.id == id && skill.visibility != SkillVisibility::Shadowed)
                    {
                        result[index].visibility = SkillVisibility::Shadowed;
                    }
                }
            }
            let prompt_path =
                policy.prompt_path_for_entry(&root, &skill_dir, &skill_file, &metadata, context);
            result.push(EffectiveSkill {
                id,
                visibility: if shadowed { SkillVisibility::Shadowed }
                    else { policy.visibility_for_root(&root, &skill_dir, &metadata, context) },
                user_invocable: policy.user_invocable(&skill_dir, &metadata, context),
                name,
                description: metadata.description,
                when_to_use: metadata.when_to_use,
                path: skill_file,
                root_path: Some(root.path.clone()),
                prompt_path,
                scope: root.scope,
                source: root.source.clone(),
            });
        }
    }
    result
}

impl AgentSkillPolicy for CodexAdapter {
    fn agent(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
        codex::roots(context, &codex_home(context))
    }

    fn is_enabled(
        &self,
        _skill_dir: &Path,
        metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> bool {
        // Enablement rules are applied once to the retained catalog, rather
        // than dropping disabled entries or reparsing User config per skill.
        metadata.has_explicit_description
    }

    fn prompt_path_for_entry(
        &self,
        _root: &SkillRoot,
        _skill_dir: &Path,
        skill_file: &Path,
        _metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> Option<String> {
        Some(skill_file.display().to_string())
    }

    fn name_collision(&self) -> NameCollision {
        NameCollision::KeepAll
    }

    fn visibility(
        &self,
        skill_dir: &Path,
        metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> SkillVisibility {
        let canonical = fs::canonicalize(&metadata.source_path).ok();
        let metadata_dir = canonical.as_deref().and_then(Path::parent).unwrap_or(skill_dir);
        if codex_manual_only(metadata_dir) {
            SkillVisibility::ManualOnly
        } else {
            SkillVisibility::Automatic
        }
    }

    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        catalog::render_codex_listing(skills, 8_000).text
    }

    fn render_initial_catalog(&self, skills: &[EffectiveSkill]) -> catalog::CatalogRender {
        catalog::render_codex_listing(skills, 8_000)
    }

    fn estimate_tokens(&self, rendered: &str) -> usize {
        approx_token_count(rendered)
    }
}

impl AgentSkillPolicy for ClaudeCodeAdapter {
    fn agent(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
        let mut roots = Vec::new();
        #[cfg(target_os = "macos")]
        {
            let managed = PathBuf::from("/Library/Application Support/ClaudeCode/.claude");
            roots.push(SkillRoot::new(managed.join("skills"), SkillScope::System));
            roots
                .push(SkillRoot::new(managed.join("commands"), SkillScope::System).flat_markdown());
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        roots.push(SkillRoot::new(
            PathBuf::from("/etc/claude-code/.claude/skills"),
            SkillScope::System,
        ));
        let user_config = claude_config_dir(context);
        roots.push(SkillRoot::new(user_config.join("skills"), SkillScope::User));
        roots.push(SkillRoot::new(user_config.join("commands"), SkillScope::User).flat_markdown());
        if !is_global_context(context) {
            for dir in cwd_to_boundary(&context.cwd, context.repository_root.as_deref()) {
                roots.push(SkillRoot::new(
                    dir.join(".claude/skills"),
                    SkillScope::Repository,
                ));
                roots.push(
                    SkillRoot::new(dir.join(".claude/commands"), SkillScope::Repository)
                        .flat_markdown(),
                );
            }
        }
        roots.extend(claude_plugin_roots(context));
        roots
    }

    fn effective_name(&self, skill_dir: &Path, metadata: &SkillMetadata) -> String {
        let local_name = claude_local_name(skill_dir, metadata);
        claude_plugin_name(skill_dir)
            .map(|plugin| format!("{plugin}:{local_name}"))
            .unwrap_or(local_name)
    }

    fn effective_name_for_root(
        &self,
        root: &SkillRoot,
        skill_dir: &Path,
        metadata: &SkillMetadata,
    ) -> String {
        let local_name = claude_local_name(skill_dir, metadata);
        root.source
            .plugin_display_name()
            .map(|plugin| format!("{plugin}:{local_name}"))
            .unwrap_or_else(|| self.effective_name(skill_dir, metadata))
    }

    fn is_enabled(
        &self,
        skill_dir: &Path,
        metadata: &SkillMetadata,
        context: &DiscoveryContext,
    ) -> bool {
        if claude_plugin_name(skill_dir).is_some() {
            return metadata.has_explicit_description || metadata.when_to_use.is_some();
        }
        claude_skill_override(context, &self.effective_name(skill_dir, metadata)).as_deref()
            != Some("off")
    }

    fn is_enabled_for_root(
        &self,
        root: &SkillRoot,
        skill_dir: &Path,
        metadata: &SkillMetadata,
        context: &DiscoveryContext,
    ) -> bool {
        if root.source.is_plugin() {
            // Claude has no separate per-Skill plugin enable switch.  The
            // plugin registry/settings already established enablement before
            // this root was created; frontmatter only controls invocation.
            metadata.has_explicit_description || metadata.when_to_use.is_some()
        } else {
            self.is_enabled(skill_dir, metadata, context)
        }
    }

    fn name_collision(&self) -> NameCollision {
        NameCollision::KeepAll
    }

    fn visibility(
        &self,
        _skill_dir: &Path,
        metadata: &SkillMetadata,
        context: &DiscoveryContext,
    ) -> SkillVisibility {
        if claude_plugin_name(_skill_dir).is_some() {
            return frontmatter_visibility(metadata);
        }
        match claude_skill_override(context, &self.effective_name(_skill_dir, metadata)).as_deref()
        {
            Some("name-only") => SkillVisibility::NameOnly,
            Some("user-invocable-only") => SkillVisibility::ManualOnly,
            Some("on") => SkillVisibility::Automatic,
            _ => frontmatter_visibility(metadata),
        }
    }

    fn visibility_for_root(
        &self,
        root: &SkillRoot,
        skill_dir: &Path,
        metadata: &SkillMetadata,
        context: &DiscoveryContext,
    ) -> SkillVisibility {
        if root.source.is_plugin() {
            frontmatter_visibility(metadata)
        } else {
            self.visibility(skill_dir, metadata, context)
        }
    }

    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        if skills.is_empty() {
            return String::new();
        }
        let entries = skills
            .iter()
            .map(|skill| -> (String, String) {
                if skill.visibility == SkillVisibility::NameOnly {
                    return (skill.name.clone(), String::new());
                }
                let description = match &skill.when_to_use {
                    Some(when_to_use) if !when_to_use.is_empty() => {
                        format!("{} - {when_to_use}", skill.description)
                    }
                    _ => skill.description.clone(),
                };
                (skill.name.clone(), truncate_chars(&description, 250))
            })
            .collect::<Vec<_>>();
        format!(
            "<system-reminder>\n{}\n</system-reminder>",
            catalog::render_claude_listing(&entries, 8_000)
        )
    }

    fn estimate_tokens(&self, rendered: &str) -> usize {
        approx_token_count(rendered)
    }
}

impl AgentSkillPolicy for CursorPolicy {
    fn agent(&self) -> AgentKind {
        AgentKind::Cursor
    }

    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
        compatible_filesystem_roots(
            context,
            &[
                ".cursor/skills",
                ".agents/skills",
                ".claude/skills",
                ".codex/skills",
            ],
            &[
                ".cursor/skills",
                ".agents/skills",
                ".claude/skills",
                ".codex/skills",
            ],
            None,
        )
    }

    fn is_enabled(
        &self,
        _skill_dir: &Path,
        metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> bool {
        compatible_skill_is_valid(metadata)
    }

    fn name_collision(&self) -> NameCollision {
        NameCollision::FirstWins
    }

    fn visibility(
        &self,
        _skill_dir: &Path,
        metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> SkillVisibility {
        frontmatter_visibility(metadata)
    }

    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        render_name_description_catalog(skills)
    }

    fn estimate_tokens(&self, rendered: &str) -> usize {
        approx_token_count(rendered)
    }
}

impl AgentSkillPolicy for OpenCodePolicy {
    fn agent(&self) -> AgentKind {
        AgentKind::OpenCode
    }

    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
        let config_home = opencode_config_home(context);
        let mut roots = Vec::new();
        let ancestor_dirs = if is_global_context(context) {
            Vec::new()
        } else {
            cwd_to_boundary(&context.cwd, context.repository_root.as_deref())
        };
        let disable_external = env_truthy("OPENCODE_DISABLE_EXTERNAL_SKILLS");
        let disable_claude = env_truthy("OPENCODE_DISABLE_CLAUDE_CODE")
            || env_truthy("OPENCODE_DISABLE_CLAUDE_CODE_SKILLS");
        if !disable_external {
            if !disable_claude {
                roots.push(
                    SkillRoot::new(context.home.join(".claude/skills"), SkillScope::User)
                        .with_root_markdown(),
                );
                for dir in ancestor_dirs.iter().rev() {
                    roots.push(
                        SkillRoot::new(dir.join(".claude/skills"), SkillScope::Repository)
                            .with_root_markdown(),
                    );
                }
            }
            roots.push(
                SkillRoot::new(context.home.join(".agents/skills"), SkillScope::User)
                    .with_root_markdown(),
            );
            for dir in ancestor_dirs.iter().rev() {
                roots.push(
                    SkillRoot::new(dir.join(".agents/skills"), SkillScope::Repository)
                        .with_root_markdown(),
                );
            }
        }
        for relative in ["skills", "skill"] {
            roots.push(
                SkillRoot::new(config_home.join(relative), SkillScope::User).with_root_markdown(),
            );
        }
        for dir in ancestor_dirs.iter().rev() {
            for relative in ["skills", "skill"] {
                roots.push(
                    SkillRoot::new(dir.join(".opencode").join(relative), SkillScope::Repository)
                        .with_root_markdown(),
                );
            }
        }
        if let Some(custom) = env::var_os("OPENCODE_CONFIG_DIR").map(PathBuf::from) {
            for relative in ["skill", "skills"] {
                roots.push(
                    SkillRoot::new(custom.join(relative), SkillScope::User).with_root_markdown(),
                );
            }
        }
        #[cfg(target_os = "macos")]
        for relative in ["skill", "skills"] {
            roots.push(
                SkillRoot::new(
                    PathBuf::from("/Library/Application Support/opencode").join(relative),
                    SkillScope::System,
                )
                .with_root_markdown(),
            );
        }
        roots.extend(
            opencode_config(context)
                .skill_paths
                .into_iter()
                .map(|(path, scope)| SkillRoot::new(path, scope).with_root_markdown()),
        );
        roots
    }

    fn is_enabled(
        &self,
        _skill_dir: &Path,
        _metadata: &SkillMetadata,
        context: &DiscoveryContext,
    ) -> bool {
        opencode_skill_permission(context, "*") != "deny"
    }

    fn is_enabled_for_entry(
        &self,
        root: &SkillRoot,
        skill_dir: &Path,
        skill_file: &Path,
        _metadata: &SkillMetadata,
        context: &DiscoveryContext,
    ) -> bool {
        opencode_skill_permission(context, &path_derived_skill_id(root, skill_dir, skill_file))
            != "deny"
    }

    fn effective_id_for_root(
        &self,
        root: &SkillRoot,
        skill_dir: &Path,
        skill_file: &Path,
        _metadata: &SkillMetadata,
    ) -> String {
        path_derived_skill_id(root, skill_dir, skill_file)
    }

    fn name_collision(&self) -> NameCollision {
        NameCollision::LastWins
    }

    fn visibility(
        &self,
        _skill_dir: &Path,
        metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> SkillVisibility {
        opencode_visibility(metadata)
    }

    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        if skills.is_empty() {
            return String::new();
        }
        let body = skills
            .iter()
            .map(|skill| {
                format!(
                    "  <skill>\n    <id>{}</id>\n    <name>{}</name>\n    <description>{}</description>\n  </skill>",
                    xml_escape(&skill.id),
                    xml_escape(&skill.name),
                    xml_escape(&skill.description)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Skills provide specialized instructions and workflows for specific tasks.\nUse the skill tool to load a skill when a task matches its description.\n<available_skills>\n{body}\n</available_skills>"
        )
    }

    fn estimate_tokens(&self, rendered: &str) -> usize {
        approx_token_count(rendered)
    }
}

impl AgentSkillPolicy for CopilotPolicy {
    fn agent(&self) -> AgentKind {
        AgentKind::Copilot
    }

    fn roots(&self, _context: &DiscoveryContext) -> Vec<SkillRoot> {
        self.roots.clone()
    }

    fn is_enabled(
        &self,
        _skill_dir: &Path,
        metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> bool {
        compatible_skill_is_valid(metadata)
    }

    fn name_collision(&self) -> NameCollision {
        NameCollision::KeepAll
    }

    fn visibility(
        &self,
        _skill_dir: &Path,
        metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> SkillVisibility {
        frontmatter_visibility(metadata)
    }

    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        render_name_description_catalog(skills)
    }

    fn estimate_tokens(&self, rendered: &str) -> usize {
        approx_token_count(rendered)
    }
}

pub(crate) fn copilot_live_config_root(home: &Path, raw: &str) -> anyhow::Result<PathBuf> {
    let cwd = env::current_dir().map_err(|_| anyhow::anyhow!("无法核验 Copilot 当前目录"))?;
    copilot_origin::config_root(home, &cwd, raw)
}

fn copilot_roots(context: &DiscoveryContext) -> anyhow::Result<Vec<SkillRoot>> {
    let configured = env::var_os("COPILOT_HOME");
    let extra = env::var_os("COPILOT_SKILLS_DIRS");
    copilot_roots_with_config(context, configured.as_deref(), extra.as_deref(),
        |hint| copilot_origin::user_roots(&context.home, &context.cwd, hint))
}

/// Relative overrides are registry hints only, including a literal `~` segment.
fn copilot_roots_with_config(
    context: &DiscoveryContext,
    configured: Option<&std::ffi::OsStr>,
    extra: Option<&std::ffi::OsStr>,
    origin: impl FnOnce(&Path) -> anyhow::Result<Vec<PathBuf>>,
) -> anyhow::Result<Vec<SkillRoot>> {
    let configured = configured.map(|value| value.to_str()
        .ok_or_else(|| anyhow::anyhow!("Copilot 配置目录编码无效")))
        .transpose()?;
    let extra = extra.map(|value| value.to_str()
        .ok_or_else(|| anyhow::anyhow!("Copilot 自定义技能目录编码无效")))
        .transpose()?;
    if let Some(raw) = configured.filter(|raw| !raw.is_empty()) {
        anyhow::ensure!(!raw.contains('\0'), "Copilot 配置目录无效");
        if !Path::new(raw).is_absolute() {
            let hint = copilot_origin::relative_config_hint(&context.cwd, raw)?;
            let settings = read_copilot_settings(&hint.join("settings.json"))?;
            copilot_custom_root_inputs(&settings, extra)?;
            return copilot_native_roots(context, origin(&hint)?);
        }
    }
    let (config_root, include_personal_agents) =
        crate::agents::copilot_config_selection_from(&context.home, configured)?;
    let settings = read_copilot_settings(&config_root.join("settings.json"))?;
    copilot_roots_with_origin(context, &config_root, &settings, extra, include_personal_agents,
        || origin(&config_root))
}

/// Read one explicit settings location; fixtures need no process environment.
#[cfg(test)]
fn copilot_roots_at(
    context: &DiscoveryContext,
    config_root: &Path,
    extra: Option<&str>,
    include_personal_agents: bool,
) -> anyhow::Result<Vec<SkillRoot>> {
    let settings = read_copilot_settings(&config_root.join("settings.json"))?;
    copilot_roots_with_origin(context, config_root, &settings, extra, include_personal_agents,
        || copilot_origin::user_roots(&context.home, &context.cwd, config_root))
}

fn copilot_roots_with_origin(
    context: &DiscoveryContext,
    config_root: &Path,
    settings: &serde_json::Value,
    extra: Option<&str>,
    include_personal_agents: bool,
    origin: impl FnOnce() -> anyhow::Result<Vec<PathBuf>>,
) -> anyhow::Result<Vec<SkillRoot>> {
    let configured = copilot_custom_root_inputs(settings, extra)?;
    if configured.iter().any(|path| !Path::new(path).is_absolute()) {
        return copilot_native_roots(context, origin()?);
    }
    copilot_roots_from(context, config_root, settings, extra, include_personal_agents)
}

fn copilot_native_roots(context: &DiscoveryContext, user_roots: Vec<PathBuf>) -> anyhow::Result<Vec<SkillRoot>> {
    anyhow::ensure!(!user_roots.is_empty() && user_roots.iter().all(|path| path.is_absolute()),
        "Copilot 原生目录来源无效");
    let mut roots = compatible_filesystem_roots(
        context, &[".github/skills", ".agents/skills", ".claude/skills"], &[], None);
    roots.extend(user_roots.into_iter().map(|path| SkillRoot::new(path, SkillScope::User)));
    Ok(roots)
}

fn read_copilot_settings(path: &Path) -> anyhow::Result<serde_json::Value> {
    match fs::metadata(path) {
        Ok(metadata) => anyhow::ensure!(metadata.is_file(), "Copilot 设置不是普通文件"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(serde_json::json!({})),
        Err(_) => anyhow::bail!("Copilot 设置无法读取"),
    }
    let bytes = read_regular_file_bytes(path, || {}).map_err(|error| match error {
        RegularFileReadError::NotRegular => anyhow::anyhow!("Copilot 设置不是普通文件"),
        RegularFileReadError::TooLarge => anyhow::anyhow!("Copilot 设置超过读取上限"),
        RegularFileReadError::Unavailable => anyhow::anyhow!("Copilot 设置无法读取"),
    })?.bytes;
    if let Ok(settings) = serde_json::from_slice::<serde_json::Value>(&bytes) {
        return Ok(settings);
    }
    let invalid = || anyhow::anyhow!("Copilot 设置无法解析");
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
    let settings = serde_json::from_str::<serde_json::Value>(&normalize_jsonc(text))
        .map_err(|_| invalid())?;
    // The shared normalizer removes comments, including incomplete comments.
    // Validate the original grammar too; JSON5-only forms still fail above.
    json5::from_str::<serde::de::IgnoredAny>(text).map_err(|_| invalid())?;
    Ok(settings)
}

/// Static roots only; no environment mutation or native Agent invocation.
fn copilot_roots_from(
    context: &DiscoveryContext,
    config_root: &Path,
    settings: &serde_json::Value,
    extra: Option<&str>,
    include_personal_agents: bool,
) -> anyhow::Result<Vec<SkillRoot>> {
    let custom = copilot_custom_root_inputs(settings, extra)?.into_iter()
        .map(|raw| crate::agents::copilot_config_root_from(&context.home, Some(raw)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut roots = compatible_filesystem_roots(
        context,
        &[".github/skills", ".agents/skills", ".claude/skills"],
        &[],
        None,
    );
    roots.push(SkillRoot::new(config_root.join("skills"), SkillScope::User));
    if include_personal_agents {
        roots.push(SkillRoot::new(context.home.join(".agents/skills"), SkillScope::User));
    }
    roots.extend(custom.into_iter().map(|path| SkillRoot::new(path, SkillScope::User)));
    Ok(roots)
}

fn copilot_custom_root_inputs<'a>(
    settings: &'a serde_json::Value,
    extra: Option<&'a str>,
) -> anyhow::Result<Vec<&'a str>> {
    anyhow::ensure!(settings.is_object(), "Copilot 设置格式无效");
    let mut custom = Vec::new();
    if let Some(value) = settings.get("skillDirectories") {
        if !value.is_null() {
            let paths = value.as_array()
                .ok_or_else(|| anyhow::anyhow!("Copilot 自定义技能目录格式无效"))?;
            for value in paths {
                let raw = value.as_str()
                    .ok_or_else(|| anyhow::anyhow!("Copilot 自定义技能目录格式无效"))?;
                anyhow::ensure!(!raw.is_empty() && !raw.contains('\0'), "Copilot 自定义技能目录无效");
                custom.push(raw);
            }
        }
    }
    if let Some(raw) = extra.filter(|raw| !raw.is_empty()) {
        // Actual 1.0.98 getDiscoveryPaths uses commas; colons/semicolons remain
        // ordinary path characters. Keep every component literal, without trim.
        for component in raw.split(',') {
            anyhow::ensure!(!component.is_empty() && !component.contains('\0'), "Copilot 自定义技能目录无效");
            custom.push(component);
        }
    }
    Ok(custom)
}

// Logical absolute fixture paths share a native root; relative negative inputs stay relative.
#[cfg(test)]
fn fixture_path(path: &str) -> PathBuf {
    match path.strip_prefix('/') {
        Some(relative) => std::env::temp_dir().join("pleat-skills-fixture").join(relative),
        None => PathBuf::from(path),
    }
}

#[cfg(test)]
mod copilot_root_tests {
    use super::*;

    fn context(global: bool) -> DiscoveryContext {
        let home = fixture_path("/synthetic/copilot-home");
        let cwd = if global { home.clone() } else { fixture_path("/synthetic/repository") };
        DiscoveryContext { home, cwd: cwd.clone(), repository_root: (!global).then_some(cwd) }
    }

    fn fixture_context(base: &Path) -> DiscoveryContext {
        let home = base.join("home");
        let cwd = base.join("project");
        fs::create_dir_all(home.join(".copilot")).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        DiscoveryContext { home, cwd: cwd.clone(), repository_root: Some(cwd) }
    }

    fn fixture_skill(root: &Path) {
        let skill = root.join("sentinel");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"),
            "---\nname: sentinel\ndescription: fixture\n---\n").unwrap();
    }

    #[test]
    fn relative_config_and_literal_tilde_scan_only_verified_native_roots() {
        for raw in ["relative-config", "~/literal-config"] {
            let temp = tempfile::tempdir().unwrap();
            let context = fixture_context(temp.path());
            let hint = context.cwd.join(raw);
            let native_root = temp.path().join("native-root");
            let custom = temp.path().join("custom-root");
            for root in [&hint.join("skills"), &context.home.join(".copilot/skills"),
                &context.home.join(".agents/skills"), &custom, &native_root] {
                fixture_skill(root);
            }
            fs::write(hint.join("settings.json"),
                serde_json::to_vec(&serde_json::json!({"skillDirectories":[custom]})).unwrap()).unwrap();
            let before = snapshot(temp.path());
            let roots = copilot_roots_with_config(&context, Some(std::ffi::OsStr::new(raw)), None,
                |actual_hint| {
                    assert_eq!(actual_hint, hint);
                    Ok(vec![native_root.clone()])
                }).unwrap();
            assert_eq!(roots.iter().map(|root| root.path.clone()).collect::<Vec<_>>(), vec![
                context.cwd.join(".github/skills"), context.cwd.join(".agents/skills"),
                context.cwd.join(".claude/skills"), native_root.clone(),
            ]);
            let estimate = adapters::inspect_copilot_roots(&context, Ok(roots));
            assert!(estimate.diagnostic.is_none());
            assert_eq!(estimate.skills.len(), 1);
            assert!(estimate.skills[0].path.starts_with(&native_root));
            assert_eq!(snapshot(temp.path()), before);
        }
    }

    #[test]
    fn relative_config_origin_failure_or_invalid_roots_never_fall_back() {
        let temp = tempfile::tempdir().unwrap();
        let context = fixture_context(temp.path());
        let hint = context.cwd.join("relative-config");
        fixture_skill(&hint.join("skills"));
        fixture_skill(&context.home.join(".copilot/skills"));
        fixture_skill(&context.cwd.join(".github/skills"));
        let before = snapshot(temp.path());
        for result in [Err(anyhow::anyhow!("native origin unavailable")), Ok(vec![]),
            Ok(vec![PathBuf::from("relative-root")]),
            Ok(vec![hint.join("skills"), PathBuf::from("relative-root")])] {
            let roots = copilot_roots_with_config(&context,
                Some(std::ffi::OsStr::new("relative-config")), None, |actual_hint| {
                    assert_eq!(actual_hint, hint);
                    result
                });
            let estimate = adapters::inspect_copilot_roots(&context, roots);
            assert_unknown(&estimate);
        }
        assert_eq!(snapshot(temp.path()), before);
    }

    #[test]
    fn invalid_relative_config_never_queries_native_origin() {
        let temp = tempfile::tempdir().unwrap();
        let context = fixture_context(temp.path());
        let config = context.cwd.join("relative-config");
        fs::create_dir_all(&config).unwrap();
        for (raw, settings, extra) in [
            ("bad\0config", "{}", None),
            ("relative-config", "{", None),
            ("relative-config", "[]", None),
            ("relative-config", r#"{"skillDirectories":["relative",17]}"#, None),
            ("relative-config", r#"{"skillDirectories":"relative"}"#, None),
            ("relative-config", r#"{"skillDirectories":["bad\u0000path"]}"#, None),
            ("relative-config", "{}", Some("relative,,extra")),
            ("relative-config", "{}", Some("bad\0extra")),
        ] {
            fs::write(config.join("settings.json"), settings).unwrap();
            let before = snapshot(temp.path());
            let roots = copilot_roots_with_config(&context, Some(std::ffi::OsStr::new(raw)),
                extra.map(std::ffi::OsStr::new), |_| panic!("invalid config reached native origin"));
            assert_unknown(&adapters::inspect_copilot_roots(&context, roots));
            assert_eq!(snapshot(temp.path()), before);
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let invalid = std::ffi::OsStr::from_bytes(b"bad\xffpath");
            for (raw, extra) in [(invalid, None),
                (std::ffi::OsStr::new("relative-config"), Some(invalid))] {
                let roots = copilot_roots_with_config(&context, Some(raw), extra,
                    |_| panic!("invalid encoding reached native origin"));
                assert_unknown(&adapters::inspect_copilot_roots(&context, roots));
            }
        }
    }

    #[test]
    fn relative_settings_use_verified_native_roots_instead_of_guessing() {
        let temp = tempfile::tempdir().unwrap();
        let context = fixture_context(temp.path());
        let native_root = temp.path().join("native-root");
        fixture_skill(&native_root);
        fixture_skill(&context.cwd.join("relative-root"));
        let roots = copilot_roots_with_origin(&context, &context.home.join(".copilot"),
            &serde_json::json!({"skillDirectories":["relative-root"]}), None, true,
            || Ok(vec![native_root.clone()]));
        let estimate = adapters::inspect_copilot_roots(&context, roots);
        assert!(estimate.diagnostic.is_none());
        assert_eq!(estimate.skills.len(), 1);
        assert!(estimate.skills[0].path.starts_with(native_root));
    }

    #[test]
    fn malformed_relative_configuration_never_queries_native_origin() {
        let context = context(false);
        for settings in [serde_json::json!({"skillDirectories":["relative",17]}),
            serde_json::json!({"skillDirectories":["relative","bad\u{0}path"]}),
            serde_json::json!({"skillDirectories":"relative"})] {
            assert!(copilot_roots_with_origin(&context, &context.home.join(".copilot"),
                &settings, None, true, || panic!("invalid settings reached native origin")).is_err());
        }
    }

    #[test]
    fn absent_native_origin_preserves_unknown_and_absolute_roots_stay_static() {
        let temp = tempfile::tempdir().unwrap();
        let context = fixture_context(temp.path());
        let config = context.home.join(".copilot");
        fixture_skill(&config.join("skills"));
        let roots = copilot_roots_with_origin(&context, &config,
            &serde_json::json!({"skillDirectories":["relative"]}), None, true,
            || anyhow::bail!("native origin unavailable"));
        assert_unknown(&adapters::inspect_copilot_roots(&context, roots));
        let roots = copilot_roots_with_origin(&context, &config, &serde_json::json!({}),
            None, true, || panic!("absolute configuration queried a native endpoint")).unwrap();
        assert!(roots.iter().any(|root| root.path == config.join("skills")));
    }

    #[test]
    fn copilot_jsonc_settings_discover_custom_skills_without_changing_files() {
        let temp = tempfile::tempdir().unwrap();
        let context = fixture_context(temp.path());
        let config = context.home.join(".copilot");
        let custom = temp.path().join("custom-skills");
        fixture_skill(&custom);
        fs::write(config.join("settings.json"), format!(
            "{{\n // configured skills\n \"skillDirectories\": [{},], /* trailing comma */\n \
             \"unrelated\": \"https://example.invalid/a/*literal*/\",\n}}\n",
            serde_json::to_string(&custom).unwrap(),
        )).unwrap();
        let before = snapshot(temp.path());
        let settings = read_copilot_settings(&config.join("settings.json")).unwrap();
        assert_eq!(settings["unrelated"], "https://example.invalid/a/*literal*/");
        let estimate = adapters::inspect_copilot_roots(&context,
            copilot_roots_at(&context, &config, None, true));
        assert!(estimate.diagnostic.is_none());
        assert_eq!(estimate.skills.len(), 1);
        assert_eq!(estimate.skills[0].name, "sentinel");
        assert_eq!(snapshot(temp.path()), before);
    }

    #[test]
    fn copilot_invalid_jsonc_keeps_counts_unknown_and_preserves_files() {
        for text in [
            "{\"skillDirectories\":[]}/*",
            "{\"skillDirectories\":[], \"unrelated\": 1/* gap */2}",
            "{skillDirectories: []}",
            "{\"skillDirectories\":['/synthetic/root']}",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let context = fixture_context(temp.path());
            let config = context.home.join(".copilot");
            fixture_skill(&config.join("skills"));
            fs::write(config.join("settings.json"), text).unwrap();
            let before = snapshot(temp.path());
            let estimate = adapters::inspect_copilot_roots(&context,
                copilot_roots_at(&context, &config, None, true));
            assert_unknown(&estimate);
            assert_eq!(snapshot(temp.path()), before);
        }
    }

    #[cfg(unix)]
    #[test]
    fn copilot_default_pair_selection_is_independent_of_root_input_order() {
        let temp = tempfile::tempdir().unwrap();
        let context = fixture_context(temp.path());
        let config = context.home.join(".copilot");
        fixture_skill(&config.join("skills"));
        fixture_skill(&context.cwd.join(".github/skills"));
        let roots = copilot_roots_at(&context, &config, None, true).unwrap();
        let estimate = adapters::inspect_copilot_roots(&context, Ok(roots.clone()));
        assert_eq!(estimate.skills.len(), 2);
        assert!(estimate.diagnostic.is_none());
        assert_eq!(estimate.model_visible_count, 1);
        assert_eq!(estimate.skills.iter().filter(|skill| skill.scope == SkillScope::Repository
            && skill.visibility == SkillVisibility::Automatic).count(), 1);
        assert_eq!(estimate.skills.iter().filter(|skill| skill.scope == SkillScope::User
            && skill.visibility == SkillVisibility::Shadowed).count(), 1);
        let reordered = adapters::inspect_copilot_roots(&context, Ok(roots.into_iter().rev().collect()));
        assert_eq!(serde_json::to_value(&estimate).unwrap(), serde_json::to_value(&reordered).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn standard_same_name_copilot_sources_select_project_and_aliases_merge() {
        let temp = tempfile::tempdir().unwrap();
        let context = fixture_context(temp.path());
        let config = context.home.join(".copilot");
        fixture_skill(&config.join("skills"));
        fixture_skill(&context.cwd.join(".github/skills"));
        let aliases = context.home.join(".agents/skills");
        fs::create_dir_all(&aliases).unwrap();
        std::os::unix::fs::symlink(config.join("skills/sentinel"), aliases.join("sentinel")).unwrap();
        let before = snapshot(temp.path());
        let estimate = adapters::inspect_copilot_roots(&context,
            copilot_roots_at(&context, &config, None, true));
        assert_eq!(estimate.skills.len(), 2);
        assert!(estimate.diagnostic.is_none());
        assert_eq!(estimate.model_visible_count, 1);
        for skill in &estimate.skills {
            let project = skill.scope == SkillScope::Repository;
            assert_eq!(skill.visibility, if project { SkillVisibility::Automatic } else { SkillVisibility::Shadowed });
            assert_eq!(skill.root_path.as_ref(), Some(&if project {
                context.cwd.join(".github/skills")
            } else { config.join("skills") }));
        }
        assert_ne!(estimate.skills[0].path.canonicalize().unwrap(),
            estimate.skills[1].path.canonicalize().unwrap());
        let mut reversed = copilot_roots_at(&context, &config, None, true).unwrap();
        reversed.reverse();
        let reordered = adapters::inspect_copilot_roots(&context, Ok(reversed));
        assert_eq!(serde_json::to_value(&estimate).unwrap(), serde_json::to_value(&reordered).unwrap());
        assert_eq!(snapshot(temp.path()), before);
    }

    #[cfg(unix)]
    #[test]
    fn copilot_same_source_alias_is_not_a_name_collision() {
        let temp = tempfile::tempdir().unwrap();
        let context = fixture_context(temp.path());
        let config = context.home.join(".copilot");
        fixture_skill(&config.join("skills"));
        let aliases = context.home.join(".agents/skills");
        fs::create_dir_all(&aliases).unwrap();
        std::os::unix::fs::symlink(config.join("skills/sentinel"), aliases.join("sentinel")).unwrap();
        let estimate = adapters::inspect_copilot_roots(&context,
            copilot_roots_at(&context, &config, None, true));
        assert_eq!(estimate.skills.len(), 1);
        assert_eq!(estimate.skills[0].visibility, SkillVisibility::Automatic);
        assert!(estimate.diagnostic.is_none());
    }

    fn assert_collision_unknown(estimate: &AgentContextEstimate) {
        assert_eq!(estimate.diagnostic.as_ref().unwrap().code, "copilot_same_name_selection_unknown");
        let json = serde_json::to_value(estimate).unwrap();
        for field in ["discovered_count", "model_visible_count", "manual_only_count",
            "name_only_count", "conditional_count", "estimated_tokens"] {
            assert!(json[field].is_null(), "{field} must stay unknown");
        }
    }

    #[cfg(unix)]
    #[test]
    fn copilot_directory_retarget_after_grouping_preserves_snapshot_and_unknown_counts() {
        for mode in ["default", "manual", "conditional"] {
            for reverse in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let context = fixture_context(temp.path());
                let config = context.home.join(".copilot");
                fixture_skill(&config.join("skills"));
                let personal_dir = config.join("skills/sentinel");
                let extension = match mode {
                    "manual" => "disable-model-invocation: true\n",
                    "conditional" => "paths: ['src/**']\n",
                    _ => "",
                };
                fs::write(personal_dir.join("SKILL.md"),
                    format!("---\nname: sentinel\ndescription: fixture\n{extension}---\n")).unwrap();
                let project_source = temp.path().join("source-a");
                fixture_skill(&project_source);
                let project_root = context.cwd.join(".github/skills");
                fs::create_dir_all(&project_root).unwrap();
                let project_alias = project_root.join("sentinel");
                std::os::unix::fs::symlink(project_source.join("sentinel"), &project_alias).unwrap();
                let mut roots = copilot_roots_at(&context, &config, None, true).unwrap();
                if reverse { roots.reverse(); }
                let reached = std::cell::Cell::new(false);
                let estimate = adapters::inspect_copilot_roots_after_grouping(&context, Ok(roots), || {
                    // This synchronous checkpoint runs after canonical grouping
                    // and identity capture, before the first bounded-header open.
                    reached.set(true);
                    assert_ne!(project_alias.canonicalize().unwrap(), personal_dir.canonicalize().unwrap());
                    fs::remove_file(&project_alias).unwrap();
                    std::os::unix::fs::symlink(&personal_dir, &project_alias).unwrap();
                });
                assert!(reached.get());
                assert_eq!(estimate.skills.len(), 2, "retain captured rows: {mode}/{reverse}");
                assert_collision_unknown(&estimate);
                for skill in &estimate.skills {
                    let expected = match (skill.scope, mode) {
                        (SkillScope::User, "manual") => SkillVisibility::ManualOnly,
                        (SkillScope::User, "conditional") => SkillVisibility::Conditional,
                        _ => SkillVisibility::Unverified,
                    };
                    assert_eq!(skill.visibility, expected, "{mode}/{reverse}");
                }
                // A separate stable inspection may collapse the now-shared
                // source; never represent the raced snapshot as a verified pair.
                if mode == "default" {
                    let stable = adapters::inspect_copilot_roots(&context,
                        copilot_roots_at(&context, &config, None, true));
                    assert_eq!(stable.skills.len(), 1);
                    assert_eq!(stable.skills[0].visibility, SkillVisibility::Automatic);
                    assert!(stable.diagnostic.is_none());
                }
            }
        }
    }

    #[test]
    fn copilot_unverified_sources_and_modes_keep_unknown_counts() {
        for variant in ["three_sources", "custom", "inherited", "plugin", "wrong_scope",
            "manual", "conditional", "user_disabled", "enabled_false", "explicit_default", "required"] {
            let temp = tempfile::tempdir().unwrap();
            let mut context = fixture_context(temp.path());
            let config = context.home.join(".copilot");
            fixture_skill(&config.join("skills"));
            fixture_skill(&context.cwd.join(".github/skills"));
            if variant == "three_sources" {
                fixture_skill(&context.cwd.join(".agents/skills"));
            }
            if variant == "inherited" {
                context.repository_root = Some(temp.path().to_path_buf());
            }
            let mut roots = copilot_roots_at(&context, &config, None, true).unwrap();
            match variant {
                "custom" => roots.push(SkillRoot::new(temp.path().join("custom"), SkillScope::User)),
                "plugin" => roots[0].source = SkillSource::Plugin { id: "fixture".into(), display_name: "fixture".into() },
                "wrong_scope" => roots[0].scope = SkillScope::User,
                "manual" | "conditional" | "user_disabled" | "enabled_false" | "explicit_default" | "required" => {
                    let mode = match variant {
                        "manual" => "disable-model-invocation: true",
                        "conditional" => "paths: ['src/**']",
                        "user_disabled" => "user-invocable: false",
                        "enabled_false" => "enabled: false",
                        "explicit_default" => "disable-model-invocation: false",
                        _ => "mode: required",
                    };
                    fs::write(config.join("skills/sentinel/SKILL.md"),
                        format!("---\nname: sentinel\ndescription: fixture\n{mode}\n---\n")).unwrap();
                },
                _ => {},
            }
            let before = snapshot(temp.path());
            let estimate = adapters::inspect_copilot_roots(&context, Ok(roots));
            assert_eq!(estimate.skills.len(), if variant == "three_sources" { 3 } else { 2 }, "{variant}");
            assert_collision_unknown(&estimate);
            for skill in &estimate.skills {
                let expected = if skill.scope == SkillScope::User && variant == "manual" {
                    SkillVisibility::ManualOnly
                } else if skill.scope == SkillScope::User && variant == "conditional" {
                    SkillVisibility::Conditional
                } else { SkillVisibility::Unverified };
                assert_eq!(skill.visibility, expected, "{variant}: {:?}", skill.scope);
            }
            assert!(!estimate.skills.iter().any(|skill| matches!(skill.visibility,
                SkillVisibility::Automatic | SkillVisibility::Shadowed)));
            assert_eq!(snapshot(temp.path()), before, "{variant}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn copilot_same_skill_file_alias_deduplicates_before_collision_selection() {
        let temp = tempfile::tempdir().unwrap();
        let context = fixture_context(temp.path());
        let config = context.home.join(".copilot");
        fixture_skill(&config.join("skills"));
        let project_skill = context.cwd.join(".github/skills/sentinel");
        fs::create_dir_all(&project_skill).unwrap();
        std::os::unix::fs::symlink(config.join("skills/sentinel/SKILL.md"),
            project_skill.join("SKILL.md")).unwrap();
        let estimate = adapters::inspect_copilot_roots(&context,
            copilot_roots_at(&context, &config, None, true));
        assert_eq!(estimate.skills.len(), 1);
        assert_eq!(estimate.skills[0].visibility, SkillVisibility::Automatic);
        assert!(estimate.diagnostic.is_none());
    }

    fn snapshot(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut result = BTreeMap::new();
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                result.insert(path.clone(), Vec::new());
                result.extend(snapshot(&path));
            } else {
                result.insert(path.clone(), fs::read(path).unwrap());
            }
        }
        result
    }

    fn assert_unknown(estimate: &AgentContextEstimate) {
        assert_eq!(estimate.agent, AgentKind::Copilot);
        assert!(estimate.skills.is_empty());
        let diagnostic = estimate.diagnostic.as_ref().unwrap();
        assert_eq!(diagnostic.code, "copilot_roots_unavailable");
        assert_eq!(diagnostic.message, "Copilot 技能目录配置无法解析或读取；技能数量未知");
        let json = serde_json::to_value(estimate).unwrap();
        assert!(json["diagnostic"].is_object());
        for field in ["discovered_count", "model_visible_count", "manual_only_count",
            "name_only_count", "conditional_count", "estimated_tokens"] {
            assert!(json.get(field).unwrap().is_null(), "{field} must be unknown");
        }
    }

    #[test]
    fn settings_failures_are_unknown_without_default_scan_or_file_changes() {
        for failure in ["invalid_json", "oversized", "unsupported_roots", "unreadable"] {
            let temp = tempfile::tempdir().unwrap();
            let context = fixture_context(temp.path());
            let config = context.home.join(".copilot");
            fixture_skill(&config.join("skills"));
            fixture_skill(&context.home.join(".agents/skills"));
            fixture_skill(&context.cwd.join(".github/skills"));
            // Existing archive and installation/source/local data are read-only inputs.
            fs::write(context.cwd.join("project.json"), b"{\"archived\":false}").unwrap();
            for directory in ["installation", "source", "local-data"] {
                let path = context.cwd.join(directory);
                fs::create_dir_all(&path).unwrap();
                fs::write(path.join("sentinel"), b"unchanged").unwrap();
            }
            let settings = config.join("settings.json");
            match failure {
                "invalid_json" => fs::write(&settings, b"{invalid").unwrap(),
                "oversized" => fs::write(&settings, vec![b' '; 1024 * 1024 + 1]).unwrap(),
                "unsupported_roots" => fs::write(&settings,
                    br#"{"skillDirectories":["relative-root"]}"#).unwrap(),
                // A directory fails as a settings file even when tests run as root.
                "unreadable" => fs::create_dir(&settings).unwrap(),
                _ => unreachable!(),
            }
            let before = snapshot(temp.path());
            let roots = copilot_roots_at(&context, &config, None, true);
            assert!(roots.is_err(), "{failure}");
            let estimate = adapters::inspect_copilot_roots(&context, roots);
            assert_unknown(&estimate);
            assert_eq!(snapshot(temp.path()), before, "{failure} changed files");
        }
    }

    #[test]
    fn injected_root_failure_is_redacted_at_estimate_boundary() {
        let context = context(true);
        let estimate = adapters::inspect_copilot_roots(&context,
            Err(anyhow::anyhow!("synthetic-sensitive-error /private/fixture")));
        assert_unknown(&estimate);
        let serialized = serde_json::to_string(&estimate).unwrap();
        assert!(!serialized.contains("synthetic-sensitive-error"));
        assert!(!serialized.contains("/private/fixture"));
    }

    #[test]
    fn missing_settings_are_known_zero_and_success_reuses_production_scanner() {
        let temp = tempfile::tempdir().unwrap();
        let context = fixture_context(temp.path());
        let config = context.home.join(".copilot");
        let estimate = adapters::inspect_copilot_roots(&context,
            copilot_roots_at(&context, &config, None, true));
        assert!(estimate.diagnostic.is_none());
        assert!(estimate.skills.is_empty());
        let json = serde_json::to_value(&estimate).unwrap();
        for field in ["discovered_count", "model_visible_count", "manual_only_count",
            "name_only_count", "conditional_count", "estimated_tokens"] {
            assert_eq!(json[field], serde_json::json!(0));
        }
        assert!(json.get("diagnostic").is_none());
        fixture_skill(&config.join("skills"));
        let estimate = adapters::inspect_copilot_roots(&context,
            copilot_roots_at(&context, &config, None, true));
        assert!(estimate.diagnostic.is_none());
        assert_eq!(estimate.discovered_count, 1);
        assert_eq!(estimate.skills[0].name, "sentinel");
    }

    #[test]
    fn default_global_roots_exclude_personal_claude_and_project_roots() {
        let context = context(true);
        let root = crate::agents::copilot_config_root_from(&context.home, None).unwrap();
        let roots = copilot_roots_from(&context, &root, &serde_json::json!({}), None, true).unwrap();
        assert_eq!(roots.iter().map(|root| root.path.clone()).collect::<Vec<_>>(),
            vec![context.home.join(".copilot/skills"), context.home.join(".agents/skills")]);
        assert!(roots.iter().all(|root| root.scope == SkillScope::User));
    }

    #[test]
    fn nondefault_root_suppresses_only_personal_agents_and_keeps_project_roots() {
        let context = context(false);
        let root = &fixture_path("/synthetic/isolated-copilot");
        let roots = copilot_roots_from(&context, root, &serde_json::json!({}), None, false).unwrap();
        let paths = roots.iter().map(|root| root.path.clone()).collect::<Vec<_>>();
        for relative in [".github/skills", ".agents/skills", ".claude/skills"] {
            assert!(paths.contains(&context.cwd.join(relative)));
        }
        assert!(paths.contains(&root.join("skills")));
        assert!(!paths.contains(&context.home.join(".agents/skills")));
        assert!(!paths.contains(&context.home.join(".copilot/skills")));
        assert!(!paths.contains(&context.home.join(".claude/skills")));
    }

    #[test]
    fn equivalent_default_override_with_parent_component_excludes_personal_agents() {
        let context = context(true);
        let (root, personal_agents) = crate::agents::copilot_config_selection_from(&context.home,
            context.home.join(".copilot/../.copilot/").to_str()).unwrap();
        let roots = copilot_roots_from(&context, &root, &serde_json::json!({}), None, personal_agents).unwrap();
        assert_eq!(root, context.home.join(".copilot"));
        assert!(!personal_agents);
        assert!(!roots.iter().any(|root| root.path == context.home.join(".agents/skills")));
    }

    #[test]
    fn custom_absolute_roots_follow_personal_roots_without_root_markdown() {
        let context = context(true);
        let root = &fixture_path("/synthetic/isolated-copilot");
        let settings_root = fixture_path("/synthetic/settings-skills");
        let extra = fixture_path("/synthetic/environment-skills");
        let settings = serde_json::json!({"skillDirectories": [settings_root]});
        let roots = copilot_roots_from(&context, root, &settings,
            extra.to_str(), false).unwrap();
        assert_eq!(roots.iter().map(|root| root.path.clone()).collect::<Vec<_>>(), vec![
            root.join("skills"), settings_root, extra,
        ]);
        assert!(roots.iter().all(|root| !root.include_root_markdown));
    }

    #[test]
    fn unsupported_custom_forms_fail_instead_of_scanning_a_default_root() {
        let context = context(true);
        let root = &fixture_path("/synthetic/isolated-copilot");
        for settings in [serde_json::json!([]), serde_json::json!({"skillDirectories": "bad"}),
            serde_json::json!({"skillDirectories": [""]}),
            serde_json::json!({"skillDirectories": ["~/skills"]}),
            serde_json::json!({"skillDirectories": [1]})] {
            assert!(copilot_roots_from(&context, root, &settings, None, false).is_err());
        }
        let empty_component = format!("{},,{}", fixture_path("/synthetic/a").display(), fixture_path("/synthetic/b").display());
        for extra in ["relative", &empty_component] {
            assert!(copilot_roots_from(&context, root, &serde_json::json!({}), Some(extra), false).is_err());
        }
    }

    #[test]
    fn native_comma_delimiter_keeps_colons_and_semicolons_literal() {
        let context = context(true);
        let config = context.home.join(".copilot");
        let one = fixture_path("/synthetic/one");
        let two = fixture_path("/synthetic/two");
        for (extra, expected) in [
            (format!("{},{}", one.display(), two.display()), vec![one.clone(), two.clone()]),
            (format!("{}:{}", one.display(), two.display()), vec![PathBuf::from(format!("{}:{}", one.display(), two.display()))]),
            (format!("{};{}", one.display(), two.display()), vec![PathBuf::from(format!("{};{}", one.display(), two.display()))]),
        ] {
            let roots = copilot_roots_from(&context, &config, &serde_json::json!({}), Some(&extra), false).unwrap();
            assert_eq!(roots.iter().skip(1).map(|root| root.path.clone()).collect::<Vec<_>>(), expected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn special_settings_files_are_unknown_and_never_wait_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let temp = tempfile::tempdir().unwrap();
        let context = fixture_context(temp.path());
        let config = context.home.join(".copilot");
        let file = config.join("settings.json");
        let cpath = std::ffi::CString::new(file.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        let estimate = adapters::inspect_copilot_roots(&context,
            copilot_roots_at(&context, &config, None, true));
        assert_unknown(&estimate);
    }
}

impl AgentSkillPolicy for AntigravityPolicy {
    fn agent(&self) -> AgentKind {
        AgentKind::Antigravity
    }

    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
        compatible_filesystem_roots(
            context,
            &[".agents/skills", ".agent/skills"],
            &[".gemini/config/skills", ".agents/skills", ".agent/skills"],
            None,
        )
    }

    fn is_enabled(
        &self,
        _skill_dir: &Path,
        metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> bool {
        compatible_skill_is_valid(metadata)
    }

    fn name_collision(&self) -> NameCollision {
        NameCollision::FirstWins
    }

    fn visibility(
        &self,
        _skill_dir: &Path,
        _metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> SkillVisibility {
        // Antigravity documents model-routed Skills and separate user-run
        // Workflows, but no Skill frontmatter switch for manual-only use.
        SkillVisibility::Automatic
    }

    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        render_name_description_catalog(skills)
    }

    fn estimate_tokens(&self, rendered: &str) -> usize {
        approx_token_count(rendered)
    }
}

impl AgentSkillPolicy for AmpPolicy {
    fn agent(&self) -> AgentKind {
        AgentKind::Amp
    }

    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
        // Amp's documented precedence puts user-owned Agent/Amp roots ahead
        // of project roots, while the Claude-compatible project root still
        // wins over the user Claude root. Keep this provider-specific order
        // instead of using the generic project-first helper.
        let mut roots = vec![
            SkillRoot::new(context.home.join(".config/agents/skills"), SkillScope::User),
            SkillRoot::new(context.home.join(".agents/skills"), SkillScope::User),
            SkillRoot::new(context.home.join(".config/amp/skills"), SkillScope::User),
        ];
        if !is_global_context(context) {
            for dir in cwd_to_boundary(&context.cwd, context.repository_root.as_deref()) {
                roots.push(SkillRoot::new(
                    dir.join(".agents/skills"),
                    SkillScope::Repository,
                ));
                roots.push(SkillRoot::new(
                    dir.join(".claude/skills"),
                    SkillScope::Repository,
                ));
            }
        }
        roots.push(SkillRoot::new(
            context.home.join(".claude/skills"),
            SkillScope::User,
        ));
        roots.extend(amp_configured_skill_roots(context));
        roots
    }

    fn is_enabled(
        &self,
        _skill_dir: &Path,
        metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> bool {
        compatible_skill_is_valid(metadata)
    }

    fn name_collision(&self) -> NameCollision {
        NameCollision::FirstWins
    }

    fn visibility(
        &self,
        _skill_dir: &Path,
        _metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> SkillVisibility {
        // Current Amp no longer exposes user-invokable Skills, so Claude's
        // disable-model-invocation extension cannot create a manual entry.
        SkillVisibility::Automatic
    }

    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        render_name_description_catalog(skills)
    }

    fn estimate_tokens(&self, rendered: &str) -> usize {
        approx_token_count(rendered)
    }
}

impl AgentSkillPolicy for DroidPolicy {
    fn agent(&self) -> AgentKind {
        AgentKind::Droid
    }

    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
        compatible_filesystem_roots(
            context,
            &[".factory/skills", ".agents/skills", ".agent/skills"],
            &[".factory/skills", ".agents/skills", ".agent/skills"],
            None,
        )
    }

    fn is_enabled(
        &self,
        _skill_dir: &Path,
        metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> bool {
        compatible_skill_is_valid(metadata)
    }

    fn name_collision(&self) -> NameCollision {
        NameCollision::FirstWins
    }

    fn visibility(
        &self,
        _skill_dir: &Path,
        metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> SkillVisibility {
        frontmatter_visibility(metadata)
    }

    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        render_name_description_catalog(skills)
    }

    fn estimate_tokens(&self, rendered: &str) -> usize {
        approx_token_count(rendered)
    }
}

impl AgentSkillPolicy for PiAdapter {
    fn agent(&self) -> AgentKind {
        AgentKind::Pi
    }

    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
        let project_trusted = !is_global_context(context) && pi_project_is_trusted(context);
        let (project_configured, user_configured) =
            pi_configured_skill_roots(context, project_trusted);
        let mut roots = Vec::new();
        roots.extend(project_configured);
        if project_trusted {
            roots.push(
                SkillRoot::new(context.cwd.join(".pi/skills"), SkillScope::Local)
                    .with_root_markdown(),
            );
            let dirs = context
                .repository_root
                .as_deref()
                .map(|root| cwd_to_boundary(&context.cwd, Some(root)))
                .unwrap_or_else(|| context.cwd.ancestors().map(Path::to_path_buf).collect());
            let user_agents = fs::canonicalize(context.home.join(".agents/skills"))
                .unwrap_or_else(|_| context.home.join(".agents/skills"));
            for dir in dirs {
                let path = dir.join(".agents/skills");
                let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                if canonical == user_agents {
                    continue;
                }
                roots.push(SkillRoot::new(path, SkillScope::Repository));
            }
        }
        roots.extend(user_configured);
        roots.push(
            SkillRoot::new(pi_agent_dir(context).join("skills"), SkillScope::User)
                .with_root_markdown(),
        );
        roots.push(SkillRoot::new(
            context.home.join(".agents/skills"),
            SkillScope::User,
        ));
        if project_trusted {
            roots.extend(pi_package_skill_roots(
                &context.cwd.join(".pi/settings.json"),
                &context.cwd.join(".pi/npm/node_modules"),
                SkillScope::Repository,
            ));
        }
        roots.extend(pi_package_skill_roots(
            &pi_agent_dir(context).join("settings.json"),
            &pi_agent_dir(context).join("npm/node_modules"),
            SkillScope::User,
        ));
        roots
    }

    fn is_enabled(
        &self,
        _skill_dir: &Path,
        metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> bool {
        metadata.has_explicit_description && !metadata.description.trim().is_empty()
    }

    fn prompt_path_for_entry(
        &self,
        _root: &SkillRoot,
        _skill_dir: &Path,
        skill_file: &Path,
        _metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> Option<String> {
        Some(skill_file.display().to_string())
    }

    fn name_collision(&self) -> NameCollision {
        NameCollision::FirstWins
    }

    fn visibility(
        &self,
        _skill_dir: &Path,
        metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> SkillVisibility {
        if frontmatter_visibility(metadata) == SkillVisibility::ManualOnly {
            SkillVisibility::ManualOnly
        } else {
            SkillVisibility::Automatic
        }
    }

    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        if skills.is_empty() {
            return String::new();
        }
        let mut lines = vec![
            "".to_string(),
            "".to_string(),
            "The following skills provide specialized instructions for specific tasks.".to_string(),
            "Use the read tool to load a skill's file when the task matches its description.".to_string(),
            "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
            "".to_string(),
            "<available_skills>".to_string(),
        ];
        for skill in skills {
            lines.push("  <skill>".to_string());
            lines.push(format!("    <name>{}</name>", xml_escape(&skill.name)));
            lines.push(format!(
                "    <description>{}</description>",
                xml_escape(&skill.description)
            ));
            lines.push(format!(
                "    <location>{}</location>",
                xml_escape(skill.prompt_path.as_deref().unwrap_or_default())
            ));
            lines.push("  </skill>".to_string());
        }
        lines.push("</available_skills>".to_string());
        lines.join("\n")
    }

    fn estimate_tokens(&self, rendered: &str) -> usize {
        approx_token_count(rendered)
    }
}

// Proma passes only its active native workspace directory to Pi. This is a
// static catalog estimate, not evidence that a running session loaded a skill.
impl AgentSkillPolicy for PromaAdapter {
    fn agent(&self) -> AgentKind {
        AgentKind::Proma
    }

    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
        proma_workspaces(context).into_iter().filter_map(|workspace| {
            let active = workspace.path.join("skills");
            (active.canonicalize().ok().as_ref() == Some(&active))
                .then(|| SkillRoot::new(active, SkillScope::Repository))
        }).collect()
    }

    fn is_enabled(&self, dir: &Path, metadata: &SkillMetadata, context: &DiscoveryContext) -> bool {
        PiAdapter.is_enabled(dir, metadata, context)
    }

    fn prompt_path_for_entry(
        &self, root: &SkillRoot, dir: &Path, file: &Path,
        metadata: &SkillMetadata, context: &DiscoveryContext,
    ) -> Option<String> {
        PiAdapter.prompt_path_for_entry(root, dir, file, metadata, context)
    }

    fn name_collision(&self) -> NameCollision {
        NameCollision::FirstWins
    }

    fn visibility(&self, dir: &Path, metadata: &SkillMetadata, context: &DiscoveryContext) -> SkillVisibility {
        PiAdapter.visibility(dir, metadata, context)
    }

    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        PiAdapter.render_visible_metadata(skills)
    }

    fn estimate_tokens(&self, rendered: &str) -> usize {
        PiAdapter.estimate_tokens(rendered)
    }
}

impl AgentSkillPolicy for GrokAdapter {
    fn agent(&self) -> AgentKind { AgentKind::Grok }

    // Effective discovery comes only from the native inspector. Skill folders
    // cannot substitute for the host's trust and effective configuration.
    fn roots(&self, _context: &DiscoveryContext) -> Vec<SkillRoot> { Vec::new() }
    fn name_collision(&self) -> NameCollision { NameCollision::KeepAll }
    fn visibility(&self, _skill_dir: &Path, _metadata: &SkillMetadata,
        _context: &DiscoveryContext) -> SkillVisibility { SkillVisibility::Unverified }

    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        if skills.is_empty() {
            return String::new();
        }
        catalog::render_grok_listing(skills, 400_000).text
    }

    fn render_initial_catalog(&self, skills: &[EffectiveSkill]) -> catalog::CatalogRender {
        catalog::render_grok_listing(skills, 400_000)
    }

    fn estimate_tokens(&self, rendered: &str) -> usize {
        approx_token_count(rendered)
    }
}

impl OpenClawPolicy {
    fn invocation_flag(metadata: &SkillMetadata, key: &str, fallback: bool) -> bool {
        let value = fs::read_to_string(&metadata.source_path).ok().and_then(|content|
            yaml_frontmatter(&content).and_then(|frontmatter|
                serde_yaml::from_str::<serde_yaml::Value>(frontmatter).ok()));
        match value.as_ref().and_then(|value| value.get(key)) {
            Some(serde_yaml::Value::Bool(value)) => *value,
            Some(serde_yaml::Value::String(value)) => match value.trim().to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => true,
                "false" | "0" | "no" | "off" => false,
                _ => fallback,
            },
            _ => fallback,
        }
    }

    fn roots_from_snapshot(context: &DiscoveryContext, snapshot: &crate::agents::OpenClawSnapshot) -> Vec<SkillRoot> {
        let state_dir = &snapshot.state;
        use OpenClawRootBoundary::{Contained, SkillOnly, TrustedTargets};
        let config = &snapshot.config;
        let routes = &snapshot.routes;
        let cwd = crate::project_discovery::path_identity(&context.cwd);
        let matching = routes.iter().filter(|(_, path)| crate::project_discovery::path_identity(path) == cwd).collect::<Vec<_>>();
        let selected = if matching.len() == 1 { matching.first().copied() }
            else if matching.is_empty() && routes.len() == 1 { routes.first() } else { None };
        let workspace = selected.map(|(_, path)| path.as_path()).unwrap_or(&context.cwd);
        let mut roots = Vec::new();
        if !is_global_context(context) {
            roots.push(SkillRoot::new(workspace.join("skills"), SkillScope::Local).openclaw(TrustedTargets));
            roots.push(SkillRoot::new(workspace.join(".agents/skills"), SkillScope::Local).openclaw(TrustedTargets));
        }
        if crate::project_discovery::path_identity(&state_dir) == crate::project_discovery::path_identity(&snapshot.effective_home.join(".openclaw")) {
            roots.push(SkillRoot::new(context.home.join(".agents/skills"), SkillScope::User).openclaw(SkillOnly));
        }
        roots.push(SkillRoot::new(state_dir.join("skills"), SkillScope::User).openclaw(SkillOnly));
        if !is_global_context(context) {
            if let Some((id, _)) = selected {
                let Ok(agent_dir) = crate::agents::openclaw_agent_dir(config, id, state_dir, &context.home) else { return Vec::new(); };
                roots.push(SkillRoot::new(agent_dir.join("workshop-skills"), SkillScope::User).openclaw(Contained));
            }
        }
        let bundled = env::var_os("OPENCLAW_BUNDLED_SKILLS_DIR").filter(|v| !v.is_empty())
            .map(|path| resolve_agent_path(&path.to_string_lossy(), &context.home, &context.cwd))
            .or_else(|| crate::agents::openclaw_bundled_skills_dir(&context.home));
        if let Some(path) = bundled {
            let mut root = SkillRoot::new(path, SkillScope::System).openclaw(Contained);
            root.source = SkillSource::Builtin;
            roots.push(root);
        }
        roots.extend(openclaw_plugin_roots(&config, &state_dir, context).into_iter().map(|root| root.openclaw(TrustedTargets)));
        roots.extend(openclaw_extra_skill_dirs(&config, context).into_iter().rev()
            .map(|path| SkillRoot::new(path, SkillScope::User).openclaw(TrustedTargets)));
        // An execution cwd is lower precedence than the configured agent catalog.
        // It contributes only its own roots; repository ancestors are never walked.
        if !is_global_context(context) && crate::project_discovery::path_identity(workspace) != cwd {
            roots.push(SkillRoot::new(context.cwd.join("skills"), SkillScope::Local).openclaw(TrustedTargets));
            roots.push(SkillRoot::new(context.cwd.join(".agents/skills"), SkillScope::Local).openclaw(TrustedTargets));
        }
        roots
    }

    fn entry_enabled(
        root: &SkillRoot, dir: &Path, file: &Path, metadata: &SkillMetadata,
        context: &DiscoveryContext, snapshot: &crate::agents::OpenClawSnapshot,
    ) -> bool {
        let Ok(real_dir) = dir.canonicalize() else { return false; };
        if !file.canonicalize().is_ok_and(|file| file.starts_with(&real_dir)) { return false; }
        let contained = root.path.canonicalize().is_ok_and(|root| real_dir.starts_with(root));
        let config = &snapshot.config;
        if root.source == SkillSource::Builtin && config.pointer("/skills/allowBundled").and_then(|v| v.as_array())
            .is_some_and(|names| !names.iter().any(|name| name.as_str() == Some(metadata.name.as_str()))) {
            return false;
        }
        let admitted = match root.openclaw_boundary.as_ref() {
            Some(OpenClawRootBoundary::SkillOnly) => true,
            Some(OpenClawRootBoundary::TrustedTargets) => contained || crate::agents::openclaw_target_is_trusted(
                &root.path, &real_dir, &config, &context.home),
            _ => contained,
        };
        admitted && !openclaw_disabled_skills(config).contains(&metadata.name)
    }
}

/// The native configuration is an immutable observation during one estimate.
struct OpenClawSnapshotPolicy { snapshot: crate::agents::OpenClawSnapshot }

impl AgentSkillPolicy for OpenClawSnapshotPolicy {
    fn agent(&self) -> AgentKind { AgentKind::OpenClaw }
    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
        OpenClawPolicy::roots_from_snapshot(context, &self.snapshot)
    }
    fn is_enabled_for_entry(&self, root: &SkillRoot, dir: &Path, file: &Path, metadata: &SkillMetadata, context: &DiscoveryContext) -> bool {
        OpenClawPolicy::entry_enabled(root, dir, file, metadata, context, &self.snapshot)
    }
    fn name_collision(&self) -> NameCollision { OpenClawPolicy.name_collision() }
    fn user_invocable(&self, dir: &Path, metadata: &SkillMetadata, context: &DiscoveryContext) -> bool {
        OpenClawPolicy.user_invocable(dir, metadata, context)
    }
    fn visibility(&self, dir: &Path, metadata: &SkillMetadata, context: &DiscoveryContext) -> SkillVisibility {
        OpenClawPolicy.visibility(dir, metadata, context)
    }
    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String { OpenClawPolicy.render_visible_metadata(skills) }
    fn estimate_tokens(&self, rendered: &str) -> usize { OpenClawPolicy.estimate_tokens(rendered) }
}

impl AgentSkillPolicy for OpenClawPolicy {
    fn agent(&self) -> AgentKind {
        AgentKind::OpenClaw
    }

    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
        match openclaw_config(context) {
            Ok(snapshot) => Self::roots_from_snapshot(context, &snapshot),
            Err(_) => Vec::new(),
        }
    }

    fn is_enabled_for_entry(
        &self, root: &SkillRoot, dir: &Path, file: &Path, metadata: &SkillMetadata,
        context: &DiscoveryContext,
    ) -> bool {
        openclaw_config(context).is_ok_and(|snapshot| Self::entry_enabled(root, dir, file, metadata, context, &snapshot))
    }

    fn is_enabled(&self, _: &Path, metadata: &SkillMetadata, context: &DiscoveryContext) -> bool {
        openclaw_config(context).is_ok_and(|snapshot| !openclaw_disabled_skills(&snapshot.config).contains(&metadata.name))
    }

    fn name_collision(&self) -> NameCollision {
        NameCollision::FirstWins
    }

    fn user_invocable(&self, _: &Path, metadata: &SkillMetadata, _: &DiscoveryContext) -> bool {
        Self::invocation_flag(metadata, "user-invocable", true)
    }

    fn visibility(
        &self,
        _skill_dir: &Path,
        metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> SkillVisibility {
        if Self::invocation_flag(metadata, "disable-model-invocation", false) {
            if Self::invocation_flag(metadata, "user-invocable", true) {
                SkillVisibility::ManualOnly
            } else {
                SkillVisibility::Disabled
            }
        } else {
            SkillVisibility::Automatic
        }
    }

    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        if skills.is_empty() {
            return String::new();
        }
        let body = skills
            .iter()
            .map(|skill| {
                format!(
                    "  <skill>\n    <name>{}</name>\n    <description>{}</description>\n  </skill>",
                    xml_escape(&skill.name),
                    xml_escape(&skill.description)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        truncate_chars(
            &format!("<available_skills>\n{body}\n</available_skills>"),
            30_000,
        )
    }

    fn estimate_tokens(&self, rendered: &str) -> usize {
        approx_token_count(rendered)
    }
}

impl AgentSkillPolicy for HermesPolicy {
    fn agent(&self) -> AgentKind {
        AgentKind::Hermes
    }

    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
        crate::agents::hermes_snapshot(&context.home, &context.cwd).map(|snapshot| snapshot.roots.into_iter()
            .map(|(path, tier)| SkillRoot::new(path, if tier == 0 { SkillScope::Repository } else { SkillScope::User })).collect())
            .unwrap_or_default()
    }

    fn name_collision(&self) -> NameCollision {
        NameCollision::FirstWins
    }

    fn visibility(
        &self,
        _skill_dir: &Path,
        _metadata: &SkillMetadata,
        _context: &DiscoveryContext,
    ) -> SkillVisibility {
        // Hermes currently indexes ordinary Skills for the model and does
        // not implement disable-model-invocation for standalone Skills.
        SkillVisibility::Automatic
    }

    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        if skills.is_empty() {
            return String::new();
        }
        let body = skills
            .iter()
            .map(|skill| {
                format!(
                    "- {}: {}",
                    skill.name,
                    truncate_chars(&skill.description, 57)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("<available_skills>\n{body}\n</available_skills>")
    }

    fn estimate_tokens(&self, rendered: &str) -> usize {
        approx_token_count(rendered)
    }
}

/// The desktop's Global project is represented by scanning `home` as the
/// selected path. It must not be treated as an ordinary project whose parent
/// directories contribute project-local compatibility roots.
fn is_global_context(context: &DiscoveryContext) -> bool {
    context.cwd == context.home
}

fn find_repository_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|dir| dir.join(".git").is_dir() || dir.join(".git").is_file())
        .map(Path::to_path_buf)
}

fn codex_home(context: &DiscoveryContext) -> PathBuf {
    env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| context.home.join(".codex"))
}

fn claude_config_dir(context: &DiscoveryContext) -> PathBuf {
    env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| context.home.join(".claude"))
}

fn pi_agent_dir(context: &DiscoveryContext) -> PathBuf {
    env::var_os("PI_CODING_AGENT_DIR")
        .map(|value| resolve_agent_path(&value.to_string_lossy(), &context.home, &context.home))
        .unwrap_or_else(|| context.home.join(".pi/agent"))
}

fn pi_project_is_trusted(context: &DiscoveryContext) -> bool {
    let Ok(content) = fs::read_to_string(pi_agent_dir(context).join("trust.json")) else {
        return true;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return true;
    };
    let Some(entries) = value.as_object() else {
        return true;
    };
    let mut current = fs::canonicalize(&context.cwd).unwrap_or_else(|_| context.cwd.clone());
    loop {
        if let Some(decision) = entries.get(&current.to_string_lossy().to_string()) {
            return decision.as_bool().unwrap_or(true);
        }
        let Some(parent) = current.parent() else {
            return true;
        };
        if parent == current {
            return true;
        }
        current = parent.to_path_buf();
    }
}

fn pi_configured_skill_roots(
    context: &DiscoveryContext,
    project_trusted: bool,
) -> (Vec<SkillRoot>, Vec<SkillRoot>) {
    let mut project_roots = Vec::new();
    let mut user_roots = Vec::new();
    if project_trusted {
        project_roots.extend(pi_settings_skill_roots(
            &context.cwd.join(".pi/settings.json"),
            &context.cwd.join(".pi"),
            context,
            SkillScope::Repository,
        ));
    }
    let agent_dir = pi_agent_dir(context);
    user_roots.extend(pi_settings_skill_roots(
        &agent_dir.join("settings.json"),
        &agent_dir,
        context,
        SkillScope::User,
    ));
    (project_roots, user_roots)
}

fn pi_package_skill_roots(
    settings_path: &Path,
    install_root: &Path,
    scope: SkillScope,
) -> Vec<SkillRoot> {
    let Ok(content) = fs::read_to_string(settings_path) else {
        return Vec::new();
    };
    let Ok(settings) = serde_json::from_str::<serde_json::Value>(&normalize_jsonc(&content)) else {
        return Vec::new();
    };
    let Some(packages) = settings.get("packages").and_then(|value| value.as_array()) else {
        return Vec::new();
    };
    let mut roots = Vec::new();
    for package in packages {
        let source = package
            .as_str()
            .or_else(|| package.get("source").and_then(|value| value.as_str()));
        let Some(package_name) = source.and_then(pi_npm_package_name) else {
            continue;
        };
        let package_root = install_root.join(package_name);
        let manifest = fs::read_to_string(package_root.join("package.json"))
            .ok()
            .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok());
        let entries = manifest
            .as_ref()
            .and_then(|manifest| manifest.pointer("/pi/skills"))
            .and_then(|value| value.as_array());
        if let Some(entries) = entries {
            for entry in entries.iter().filter_map(|value| value.as_str()) {
                if entry.starts_with('!') || entry.contains('*') {
                    continue;
                }
                let path = package_root.join(entry);
                roots.push(if path.is_file() {
                    SkillRoot::exact(path, scope)
                } else {
                    SkillRoot::new(path, scope).with_root_markdown()
                });
            }
        } else {
            roots.push(SkillRoot::new(package_root.join("skills"), scope).with_root_markdown());
        }
    }
    roots
}

fn pi_npm_package_name(source: &str) -> Option<&str> {
    let source = source.strip_prefix("npm:").unwrap_or(source);
    if source.starts_with("git:") || source.contains("://") || source.starts_with("github:") {
        return None;
    }
    if source.starts_with('@') {
        let slash = source.find('/')?;
        let version = source[slash + 1..]
            .find('@')
            .map(|offset| slash + 1 + offset)
            .unwrap_or(source.len());
        Some(&source[..version])
    } else {
        Some(source.split('@').next().unwrap_or(source))
    }
}

fn pi_settings_skill_roots(
    settings_path: &Path,
    relative_base: &Path,
    context: &DiscoveryContext,
    scope: SkillScope,
) -> Vec<SkillRoot> {
    let Ok(content) = fs::read_to_string(settings_path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&normalize_jsonc(&content)) else {
        return Vec::new();
    };
    let Some(entries) = value.get("skills") else {
        return Vec::new();
    };
    let entries = entries
        .as_array()
        .cloned()
        .or_else(|| {
            entries
                .get("customDirectories")
                .and_then(|value| value.as_array())
                .cloned()
        })
        .unwrap_or_default();
    entries
        .iter()
        .filter_map(|entry| entry.as_str())
        .filter(|entry| !entry.starts_with('!'))
        .map(|entry| {
            let path = resolve_agent_path(entry, &context.home, relative_base);
            if path.extension().is_some_and(|extension| extension == "md") {
                SkillRoot::exact(path, scope)
            } else {
                SkillRoot::new(path, scope).with_root_markdown()
            }
        })
        .collect()
}

fn resolve_agent_path(value: &str, home: &Path, relative_base: &Path) -> PathBuf {
    if value == "~" {
        return home.to_path_buf();
    }
    if let Some(relative) = value.strip_prefix("~/") {
        return home.join(relative);
    }
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        relative_base.join(path)
    }
}

fn compatible_filesystem_roots(
    context: &DiscoveryContext,
    project_relative: &[&str],
    user_relative: &[&str],
    extra_env: Option<&str>,
) -> Vec<SkillRoot> {
    let mut roots = Vec::new();
    if !is_global_context(context) {
        for dir in cwd_to_boundary(&context.cwd, context.repository_root.as_deref()) {
            for relative in project_relative {
                roots.push(SkillRoot::new(dir.join(relative), SkillScope::Repository));
            }
        }
    }
    for relative in user_relative {
        roots.push(SkillRoot::new(
            context.home.join(relative),
            SkillScope::User,
        ));
    }
    if let Some(name) = extra_env {
        if let Some(value) = env::var_os(name) {
            roots.extend(env::split_paths(&value).map(|path| {
                let path = if path.is_absolute() {
                    path
                } else {
                    context.cwd.join(path)
                };
                SkillRoot::new(path, SkillScope::User).with_root_markdown()
            }));
        }
    }
    roots
}

fn amp_configured_skill_roots(context: &DiscoveryContext) -> Vec<SkillRoot> {
    let mut paths = Vec::new();
    if let Some(value) = env::var_os("AMP_SKILL_PATHS") {
        paths.extend(env::split_paths(&value).map(|path| {
            if path.is_absolute() {
                path
            } else {
                context.cwd.join(path)
            }
        }));
    }
    let config_files = [
        context.home.join(".config/amp/settings.json"),
        context.home.join(".config/amp/config.json"),
        context.cwd.join(".amp/settings.json"),
        context.cwd.join(".amp/config.json"),
    ];
    for file in config_files {
        if is_global_context(context) && file.starts_with(&context.cwd.join(".amp")) {
            continue;
        }
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&normalize_jsonc(&content))
        else {
            continue;
        };
        let Some(skill_value) = value
            .pointer("/skills/path")
            .or_else(|| value.pointer("/skills/paths"))
            .or_else(|| value.get("skillPath"))
        else {
            continue;
        };
        let values = skill_value
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| skill_value.as_str().into_iter().collect::<Vec<_>>());
        paths.extend(values.into_iter().map(|path| {
            resolve_agent_path(path, &context.home, file.parent().unwrap_or(&context.cwd))
        }));
    }
    paths
        .into_iter()
        .map(|path| SkillRoot::new(path, SkillScope::User).with_root_markdown())
        .collect()
}

fn compatible_skill_is_valid(metadata: &SkillMetadata) -> bool {
    metadata.has_explicit_description
        && !metadata.description.trim().is_empty()
        && metadata.description.encode_utf16().count() <= 1_024
        && valid_agent_skill_name(&metadata.name)
}

fn render_name_description_catalog(skills: &[EffectiveSkill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let body = skills
        .iter()
        .map(|skill| {
            format!(
                "  <skill>\n    <name>{}</name>\n    <description>{}</description>\n  </skill>",
                xml_escape(&skill.name),
                xml_escape(&skill.description)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("<available_skills>\n{body}\n</available_skills>")
}

fn path_derived_skill_id(root: &SkillRoot, skill_dir: &Path, skill_file: &Path) -> String {
    if let Ok(relative) = skill_dir.strip_prefix(&root.path) {
        if let Some(name) = relative.file_name() {
            return name.to_string_lossy().into_owned();
        }
    }
    skill_file
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "SKILL".to_string())
}

fn opencode_visibility(metadata: &SkillMetadata) -> SkillVisibility {
    if !metadata.has_explicit_description || metadata.description.trim().is_empty() {
        return SkillVisibility::ManualOnly;
    }
    let Ok(content) = fs::read_to_string(&metadata.source_path) else {
        return SkillVisibility::Automatic;
    };
    let Some(frontmatter) = yaml_frontmatter(&content) else {
        return SkillVisibility::Automatic;
    };
    let Ok(parsed) = serde_yaml::from_str::<SkillFrontmatter>(frontmatter) else {
        return SkillVisibility::Automatic;
    };
    if yaml_opencode_autoinvoke_disabled(parsed.metadata.as_ref()) {
        SkillVisibility::ManualOnly
    } else {
        SkillVisibility::Automatic
    }
}

fn yaml_opencode_autoinvoke_disabled(value: Option<&serde_yaml::Value>) -> bool {
    let Some(value) = value else { return false };
    if value
        .get("opencode/autoinvoke")
        .is_some_and(|value| value.as_bool() == Some(false) || value.as_str() == Some("false"))
    {
        return true;
    }
    let Some(opencode) = value.get("opencode") else {
        return false;
    };
    let Some(autoinvoke) = opencode.get("autoinvoke") else {
        return false;
    };
    autoinvoke.as_bool() == Some(false) || autoinvoke.as_str() == Some("false")
}

fn openclaw_config(context: &DiscoveryContext) -> std::result::Result<crate::agents::OpenClawSnapshot, crate::agents::OpenClawConfigFailure> {
    crate::agents::openclaw_snapshot(&context.home)
}

fn openclaw_extra_skill_dirs(
    config: &serde_json::Value,
    context: &DiscoveryContext,
) -> Vec<PathBuf> {
    config
        .pointer("/skills/load/extraDirs")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str())
        .filter_map(|path| crate::agents::openclaw_user_path(path, &context.home).ok())
        .collect()
}

fn openclaw_disabled_skills(config: &serde_json::Value) -> HashSet<String> {
    config
        .pointer("/skills/entries")
        .and_then(|value| value.as_object())
        .into_iter()
        .flatten()
        .filter_map(|(name, entry)| {
            (entry.get("enabled").and_then(|value| value.as_bool()) == Some(false))
                .then(|| name.clone())
        })
        .collect()
}

fn openclaw_plugin_roots(
    config: &serde_json::Value,
    state_dir: &Path,
    context: &DiscoveryContext,
) -> Vec<SkillRoot> {
    if config
        .pointer("/plugins/enabled")
        .and_then(|value| value.as_bool())
        == Some(false)
    {
        return Vec::new();
    }
    let allow = config
        .pointer("/plugins/allow")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .map(str::to_string)
                .collect::<HashSet<_>>()
        });
    let deny = config
        .pointer("/plugins/deny")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str())
        .map(str::to_string)
        .collect::<HashSet<_>>();
    let mut candidates = Vec::new();
    if let Ok(entries) = fs::read_dir(state_dir.join("extensions")) {
        candidates.extend(entries.flatten().map(|entry| entry.path()));
    }
    if let Some(paths) = config
        .pointer("/plugins/load/paths")
        .and_then(|value| value.as_array())
    {
        candidates.extend(
            paths
                .iter()
                .filter_map(|value| value.as_str())
                .filter_map(|value| crate::agents::openclaw_user_path(value, &context.home).ok()),
        );
    }
    let mut seen = HashSet::new();
    let mut roots = Vec::new();
    for candidate in candidates {
        let plugin_root = if candidate.is_file() {
            candidate.parent().unwrap_or(&candidate).to_path_buf()
        } else {
            candidate
        };
        let canonical = fs::canonicalize(&plugin_root).unwrap_or_else(|_| plugin_root.clone());
        if !seen.insert(canonical) {
            continue;
        }
        let manifest_path = plugin_root.join("openclaw.plugin.json");
        let Ok(content) = fs::read_to_string(manifest_path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        let Some(id) = manifest.get("id").and_then(|value| value.as_str()) else {
            continue;
        };
        let entry_disabled = config
            .get("plugins")
            .and_then(|plugins| plugins.get("entries"))
            .and_then(|entries| entries.get(id))
            .and_then(|entry| entry.get("enabled"))
            .and_then(|value| value.as_bool())
            == Some(false);
        if deny.contains(id)
            || allow.as_ref().is_some_and(|allow| !allow.contains(id))
            || entry_disabled
        {
            continue;
        }
        let display_name = manifest
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or(id);
        let skill_paths = manifest
            .get("skills")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str());
        for relative in skill_paths {
            let relative = PathBuf::from(relative);
            if !relative.is_absolute()
                && !relative.components().any(|part| part.as_os_str() == "..")
            {
                roots.push(SkillRoot::plugin(
                    plugin_root.join(relative),
                    SkillScope::User,
                    id,
                    display_name,
                ));
            }
        }
    }
    roots
}

fn claude_skill_override(context: &DiscoveryContext, name: &str) -> Option<String> {
    claude_skill_overrides(context).remove(name)
}

fn claude_settings_files(context: &DiscoveryContext) -> Vec<PathBuf> {
    let project_root = context.repository_root.as_deref().unwrap_or(&context.cwd);
    let mut files = vec![context.home.join(".claude/settings.json")];
    files.extend([
        project_root.join(".claude/settings.json"),
        project_root.join(".claude/settings.local.json"),
    ]);
    #[cfg(target_os = "macos")]
    files.push(PathBuf::from(
        "/Library/Application Support/ClaudeCode/managed-settings.json",
    ));
    #[cfg(all(unix, not(target_os = "macos")))]
    files.push(PathBuf::from("/etc/claude-code/managed-settings.json"));
    files
}

fn claude_skill_overrides(context: &DiscoveryContext) -> HashMap<String, String> {
    let mut overrides = HashMap::new();
    for file in claude_settings_files(context) {
        let Ok(content) = fs::read_to_string(file) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        let Some(entries) = value
            .get("skillOverrides")
            .and_then(|value| value.as_object())
        else {
            continue;
        };
        for (name, state) in entries {
            if let Some(state) = state.as_str() {
                overrides.insert(name.clone(), state.to_string());
            }
        }
    }
    overrides
}

fn claude_plugin_roots(context: &DiscoveryContext) -> Vec<SkillRoot> {
    let mut enabled = HashSet::new();
    for file in claude_settings_files(context) {
        let Ok(content) = fs::read_to_string(file) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        if let Some(plugins) = value
            .get("enabledPlugins")
            .and_then(|value| value.as_object())
        {
            for (id, state) in plugins {
                if state.as_bool() == Some(true) {
                    enabled.insert(id.clone());
                } else if state.as_bool() == Some(false) {
                    enabled.remove(id);
                }
            }
        }
    }
    let path = context.home.join(".claude/plugins/installed_plugins.json");
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return Vec::new();
    };
    let Some(plugins) = value.get("plugins").and_then(|value| value.as_object()) else {
        return Vec::new();
    };
    let project_root = context.repository_root.as_deref().unwrap_or(&context.cwd);
    let global_only = is_global_context(context);
    let mut roots = Vec::new();
    for (id, installations) in plugins {
        if !enabled.contains(id) {
            continue;
        }
        let Some(installations) = installations.as_array() else {
            continue;
        };
        for installation in installations {
            let scope = installation.get("scope").and_then(|value| value.as_str());
            let applies = scope == Some("user")
                || (!global_only
                    && scope == Some("project")
                    && installation
                        .get("projectPath")
                        .and_then(|value| value.as_str())
                        .is_some_and(|path| Path::new(path) == project_root));
            if !applies {
                continue;
            }
            if let Some(path) = installation
                .get("installPath")
                .and_then(|value| value.as_str())
            {
                let scope = match scope {
                    Some("project") => SkillScope::Repository,
                    Some("user") => SkillScope::User,
                    _ => SkillScope::System,
                };
                let plugin_name = plugin_display_fallback(id);
                roots.extend(plugin_skill_roots(
                    Path::new(path),
                    ".claude-plugin/plugin.json",
                    id,
                    "skills",
                    scope,
                    plugin_name,
                ));
            }
        }
    }
    roots
}

fn claude_plugin_name(skill_dir: &Path) -> Option<String> {
    let components = skill_dir
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    components
        .windows(5)
        .find(|parts| parts[0] == ".claude" && parts[1] == "plugins" && parts[2] == "cache")
        .map(|parts| parts[4].clone())
}

fn claude_local_name(skill_dir: &Path, metadata: &SkillMetadata) -> String {
    if metadata
        .source_path
        .file_name()
        .is_some_and(|name| name != "SKILL.md")
    {
        return metadata.name.clone();
    }
    skill_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| metadata.name.clone())
}

#[derive(Default)]
struct OpenCodeConfigSnapshot {
    skill_paths: Vec<(PathBuf, SkillScope)>,
    permission_rules: Vec<(String, String)>,
}

fn opencode_config_home(context: &DiscoveryContext) -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| context.home.join(".config"))
        .join("opencode")
}

fn env_truthy(name: &str) -> bool {
    env::var(name).ok().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn opencode_config(context: &DiscoveryContext) -> OpenCodeConfigSnapshot {
    let config_home = opencode_config_home(context);
    let mut files = vec![
        (config_home.join("opencode.json"), SkillScope::User),
        (config_home.join("opencode.jsonc"), SkillScope::User),
    ];
    if let Some(custom) = env::var_os("OPENCODE_CONFIG").map(PathBuf::from) {
        files.push((custom, SkillScope::User));
    }
    if !is_global_context(context) {
        let project_configs = cwd_to_boundary(&context.cwd, context.repository_root.as_deref())
            .into_iter()
            .rev()
            .flat_map(|dir| [dir.join("opencode.json"), dir.join("opencode.jsonc")])
            .filter(|path| path.is_file());
        files.extend(project_configs.map(|path| (path, SkillScope::Repository)));
        if let Some(project_config) = env::var_os("OPENCODE_PROJECT_CONFIG").map(PathBuf::from) {
            files.push((project_config, SkillScope::Repository));
        }
    }
    #[cfg(target_os = "macos")]
    files.extend([
        (
            PathBuf::from("/Library/Application Support/opencode/opencode.json"),
            SkillScope::System,
        ),
        (
            PathBuf::from("/Library/Application Support/opencode/opencode.jsonc"),
            SkillScope::System,
        ),
    ]);

    let mut snapshot = OpenCodeConfigSnapshot::default();
    let mut default_agent = "build".to_string();
    let mut values = Vec::new();
    for (file, scope) in files {
        let Ok(content) = fs::read_to_string(file) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&normalize_jsonc(&content))
        else {
            continue;
        };
        if let Some(agent) = value.get("default_agent").and_then(|value| value.as_str()) {
            default_agent = agent.to_string();
        }
        values.push((value, scope));
    }
    for (value, scope) in values {
        let skill_paths = value
            .get("skills")
            .and_then(|skills| skills.as_array())
            .or_else(|| {
                value
                    .get("skills")
                    .and_then(|skills| skills.get("paths"))
                    .and_then(|paths| paths.as_array())
            });
        if let Some(paths) = skill_paths {
            for path in paths.iter().filter_map(|value| value.as_str()) {
                if !path.starts_with("http://") && !path.starts_with("https://") {
                    snapshot
                        .skill_paths
                        .push((expand_user_path(path, context), scope));
                }
            }
        }
        append_opencode_permission_rules(
            value
                .get("permissions")
                .or_else(|| value.get("permission").and_then(|value| value.get("skill"))),
            &mut snapshot.permission_rules,
        );
        append_opencode_permission_rules(
            value
                .get("agents")
                .or_else(|| value.get("agent"))
                .and_then(|value| value.get(&default_agent))
                .and_then(|value| {
                    value
                        .get("permissions")
                        .or_else(|| value.get("permission").and_then(|value| value.get("skill")))
                }),
            &mut snapshot.permission_rules,
        );
    }
    snapshot
}

fn append_opencode_permission_rules(
    value: Option<&serde_json::Value>,
    rules: &mut Vec<(String, String)>,
) {
    let Some(value) = value else { return };
    if let Some(action) = value.as_str() {
        rules.push(("*".to_string(), action.to_string()));
    } else if let Some(entries) = value.as_array() {
        for entry in entries {
            if entry
                .get("action")
                .and_then(|value| value.as_str())
                .is_some_and(|action| action != "skill")
            {
                continue;
            }
            let Some(resource) = entry
                .get("resource")
                .or_else(|| entry.get("pattern"))
                .and_then(|value| value.as_str())
            else {
                continue;
            };
            let Some(effect) = entry
                .get("effect")
                .or_else(|| entry.get("action"))
                .and_then(|value| value.as_str())
            else {
                continue;
            };
            rules.push((resource.to_string(), effect.to_string()));
        }
    } else if let Some(entries) = value.as_object() {
        for (pattern, action) in entries {
            if let Some(action) = action.as_str() {
                rules.push((pattern.clone(), action.to_string()));
            }
        }
    }
}

fn opencode_skill_permission(context: &DiscoveryContext, name: &str) -> String {
    let mut action = "allow".to_string();
    for (pattern, candidate) in opencode_config(context).permission_rules {
        if wildcard_match(&pattern, name) {
            action = candidate;
        }
    }
    action
}

fn expand_user_path(value: &str, context: &DiscoveryContext) -> PathBuf {
    if let Some(relative) = value.strip_prefix("~/") {
        context.home.join(relative)
    } else {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            context.cwd.join(path)
        }
    }
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let parts = pattern.split('*').collect::<Vec<_>>();
    if parts.len() == 1 {
        return pattern == value;
    }
    let mut rest = value;
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        let Some(position) = rest.find(part) else {
            return false;
        };
        if index == 0 && !pattern.starts_with('*') && position != 0 {
            return false;
        }
        rest = &rest[position + part.len()..];
    }
    pattern.ends_with('*') || rest.is_empty()
}

fn normalize_jsonc(content: &str) -> String {
    let mut without_comments = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    while let Some(ch) = chars.next() {
        if line_comment {
            if ch == '\n' {
                line_comment = false;
                without_comments.push(ch);
            }
            continue;
        }
        if block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                block_comment = false;
            }
            continue;
        }
        if in_string {
            without_comments.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            without_comments.push(ch);
        } else if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            line_comment = true;
        } else if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            block_comment = true;
        } else {
            without_comments.push(ch);
        }
    }

    let mut normalized = String::with_capacity(without_comments.len());
    let chars = without_comments.chars().collect::<Vec<_>>();
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < chars.len() {
        let ch = chars[index];
        if in_string {
            normalized.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            normalized.push(ch);
            index += 1;
            continue;
        }
        if ch == ',' {
            let mut next = index + 1;
            while next < chars.len() && chars[next].is_whitespace() {
                next += 1;
            }
            if next < chars.len() && matches!(chars[next], '}' | ']') {
                index += 1;
                continue;
            }
        }
        normalized.push(ch);
        index += 1;
    }
    normalized
}

fn plugin_skill_roots(
    plugin_root: &Path,
    manifest_relative_path: &str,
    plugin_id: &str,
    fallback_skill_path: &str,
    scope: SkillScope,
    fallback_display_name: &str,
) -> Vec<SkillRoot> {
    let manifest_path = plugin_root.join(manifest_relative_path);
    let (display_name, skill_paths) = if manifest_path.is_file() {
        let Ok(content) = fs::read_to_string(&manifest_path) else {
            return Vec::new();
        };
        let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) else {
            return Vec::new();
        };
        let display_name = manifest
            .get("interface")
            .and_then(|interface| interface.get("displayName"))
            .or_else(|| manifest.get("displayName"))
            .or_else(|| manifest.get("display_name"))
            .or_else(|| manifest.get("name"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(fallback_display_name)
            .to_string();
        let skills_value = manifest
            .get("paths")
            .and_then(|paths| paths.get("skills"))
            .or_else(|| manifest.get("skills"));
        let paths = match skills_value {
            None => vec![fallback_skill_path.to_string()],
            Some(serde_json::Value::String(path)) => vec![path.clone()],
            Some(serde_json::Value::Array(paths)) => paths
                .iter()
                .filter_map(|path| path.as_str())
                .map(str::to_string)
                .collect::<Vec<_>>(),
            // An explicitly empty or malformed contribution is not an
            // invitation to scan the entire plugin cache.
            Some(_) => Vec::new(),
        };
        (display_name, paths)
    } else {
        (
            fallback_display_name.to_string(),
            vec![fallback_skill_path.to_string()],
        )
    };
    let source = SkillSource::Plugin {
        id: plugin_id.to_string(),
        display_name,
    };
    skill_paths
        .into_iter()
        .filter_map(|relative| {
            let relative = PathBuf::from(relative);
            if relative.is_absolute() {
                return None;
            }
            let path = plugin_root.join(relative);
            if path.is_file() {
                Some(SkillRoot::exact(path, scope).with_source(source.clone()))
            } else {
                Some(SkillRoot::new(path, scope).with_source(source.clone()))
            }
        })
        .collect()
}

fn plugin_display_fallback(plugin_id: &str) -> &str {
    plugin_id
        .split_once('@')
        .map(|(plugin, _)| plugin)
        .unwrap_or(plugin_id)
}

const AGENT_PLUGIN_MANIFEST: &str = "plugin.json";
const AGENT_PLUGIN_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";
const AGENT_PLUGIN_SCHEMA_PREFIX: &str = "https://agent-plugins.org/schemas/";
const CODEX_LEGACY_PLUGIN_MANIFESTS: &[&str] = &[
    ".codex-plugin/plugin.json",
    ".claude-plugin/plugin.json",
    ".cursor-plugin/plugin.json",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodexPluginManifestFormat {
    AgentPlugin,
    Legacy,
}

fn codex_plugin_manifest(plugin_root: &Path) -> Option<(&'static str, CodexPluginManifestFormat)> {
    let agent_manifest = plugin_root.join(AGENT_PLUGIN_MANIFEST);
    match fs::symlink_metadata(&agent_manifest) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.file_type().is_file() => {
            return None;
        }
        Ok(_) => {
            if let Ok(content) = fs::read_to_string(&agent_manifest)
                && let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content)
                && let Some(schema) = manifest.get("$schema").and_then(|value| value.as_str())
            {
                if schema == AGENT_PLUGIN_SCHEMA {
                    return Some((
                        AGENT_PLUGIN_MANIFEST,
                        CodexPluginManifestFormat::AgentPlugin,
                    ));
                }
                if schema.starts_with(AGENT_PLUGIN_SCHEMA_PREFIX) {
                    return None;
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return None,
    }

    for relative_path in CODEX_LEGACY_PLUGIN_MANIFESTS {
        let manifest = plugin_root.join(relative_path);
        let parent = manifest.parent()?;
        match fs::symlink_metadata(parent) {
            Ok(metadata) if !metadata.file_type().is_dir() => return None,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        }
        match fs::symlink_metadata(&manifest) {
            Ok(metadata) if metadata.file_type().is_file() => {
                return Some((relative_path, CodexPluginManifestFormat::Legacy));
            }
            Ok(_) => return None,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    None
}

fn codex_agent_plugin_display_name(
    plugin_root: &Path,
    manifest: &serde_json::Value,
    fallback: &str,
) -> String {
    let inline_extension = manifest.pointer("/extensions/com.openai");
    let inline_display_name = inline_extension
        .and_then(|extension| extension.pointer("/interface/displayName"))
        .and_then(serde_json::Value::as_str);
    let overlay_display_name = inline_extension.is_none().then(|| {
        fs::read_to_string(plugin_root.join(".codex-plugin/plugin.json"))
            .ok()
            .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
            .and_then(|overlay| {
                overlay
                    .pointer("/interface/displayName")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
    });
    inline_display_name
        .map(str::to_string)
        .or_else(|| overlay_display_name.flatten())
        .or_else(|| {
            manifest
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn codex_plugin_skill_roots(
    plugin_root: &Path,
    plugin_id: &str,
    fallback_display_name: &str,
) -> Vec<SkillRoot> {
    let Some((manifest_relative_path, format)) = codex_plugin_manifest(plugin_root) else {
        return Vec::new();
    };
    match format {
        CodexPluginManifestFormat::AgentPlugin => {
            let Ok(content) = fs::read_to_string(plugin_root.join(manifest_relative_path)) else {
                return Vec::new();
            };
            let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) else {
                return Vec::new();
            };
            let display_name =
                codex_agent_plugin_display_name(plugin_root, &manifest, fallback_display_name);
            vec![
                SkillRoot::plugin(
                    plugin_root.join("skills"),
                    SkillScope::System,
                    plugin_id,
                    display_name,
                )
                .direct_children(),
            ]
        }
        CodexPluginManifestFormat::Legacy => plugin_skill_roots(
            plugin_root,
            manifest_relative_path,
            plugin_id,
            "skills",
            SkillScope::System,
            fallback_display_name,
        ),
    }
}

fn codex_plugin_roots(codex_home: &Path, config: &toml::Value) -> Vec<SkillRoot> {
    enabled_plugin_ids_from_value(config)
        .into_iter()
        .filter_map(|id| {
            let (plugin, marketplace) = id.rsplit_once('@')?;
            let versions = codex_home
                .join("plugins/cache")
                .join(marketplace)
                .join(plugin);
            let version = fs::read_dir(versions)
                .ok()?
                .filter_map(Result::ok)
                .filter(|entry| entry.path().is_dir())
                .max_by_key(|entry| entry.file_name())?;
            let plugin_root = version.path();
            let display_name = plugin_display_fallback(plugin);
            Some(codex_plugin_skill_roots(&plugin_root, &id, display_name))
        })
        .flatten()
        .collect()
}

#[cfg(test)]
fn enabled_plugin_ids(config: &str) -> Vec<String> {
    let Ok(config) = toml::from_str::<toml::Value>(config) else {
        return Vec::new();
    };
    enabled_plugin_ids_from_value(&config)
}

fn enabled_plugin_ids_from_value(config: &toml::Value) -> Vec<String> {
    let Some(plugins) = config.get("plugins").and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    let mut enabled = plugins
        .iter()
        .filter(|(_, plugin)| {
            plugin.as_table().is_some_and(|plugin| {
                plugin
                    .get("enabled")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(true)
            })
        })
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    enabled.sort();
    enabled
}

fn cwd_to_boundary(cwd: &Path, boundary: Option<&Path>) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for dir in cwd.ancestors() {
        result.push(dir.to_path_buf());
        if boundary.is_some_and(|root| root == dir) {
            break;
        }
    }
    result
}

fn read_metadata(path: &Path) -> Option<SkillMetadata> {
    read_metadata_with_profile(path, MetadataProfile::StrictFrontmatter)
}

fn read_metadata_with_profile(path: &Path, profile: MetadataProfile) -> Option<SkillMetadata> {
    if matches!(profile, MetadataProfile::WorkBuddy) {
        return desktop_agents::workbuddy_metadata(path);
    }
    let content = fs::read_to_string(path).ok()?;
    let fallback = if path.file_name().is_some_and(|name| name == "SKILL.md") {
        path.parent()?.file_name()?.to_string_lossy().into_owned()
    } else {
        path.file_stem()?.to_string_lossy().into_owned()
    };
    let parsed = yaml_frontmatter(&content)
        .and_then(|frontmatter| serde_yaml::from_str::<SkillFrontmatter>(frontmatter).ok());
    if matches!(profile, MetadataProfile::StrictFrontmatter) && parsed.is_none() {
        return None;
    }
    let parsed = parsed.unwrap_or_default();
    let has_explicit_description = parsed.description.is_some();
    Some(SkillMetadata {
        name: parsed
            .name
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(fallback),
        description: parsed.description.unwrap_or_else(|| {
            if matches!(profile, MetadataProfile::ProviderFrontmatter) {
                String::new()
            } else {
                first_body_paragraph(&content)
            }
        }),
        has_explicit_description,
        when_to_use: parsed.when_to_use,
        source_path: path.to_path_buf(),
    })
}

fn first_body_paragraph(content: &str) -> String {
    let has_frontmatter = yaml_frontmatter(content).is_some();
    let mut delimiters = if has_frontmatter { 0 } else { 2 };
    let mut paragraph = Vec::new();
    for line in content.lines() {
        if has_frontmatter && line.trim() == "---" && delimiters < 2 {
            delimiters += 1;
            continue;
        }
        if delimiters < 2 {
            continue;
        }
        if line.trim().is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        paragraph.push(line.trim());
    }
    paragraph.join(" ")
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut result = value
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    result.push('…');
    result
}

/// Lightweight, provider-independent estimate used when an exact tokenizer is
/// not available.  Every provider passes its final rendered catalog here, so
/// fixed prompt markup, escaping and path fields are included consistently.
fn approx_token_count(rendered: &str) -> usize {
    rendered.len().div_ceil(4)
}

fn valid_agent_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn yaml_frontmatter(content: &str) -> Option<&str> {
    let rest = content
        .strip_prefix("---\r\n")
        .or_else(|| content.strip_prefix("---\n"))?;
    let lf = rest.find("\n---\n").map(|at| (at, 5));
    let crlf = rest.find("\r\n---\r\n").map(|at| (at, 9));
    let eof = rest.strip_suffix("\n---").map(|body| (body.len(), 4));
    let (end, _) = [lf, crlf, eof]
        .into_iter()
        .flatten()
        .min_by_key(|(at, _)| *at)?;
    Some(&rest[..end])
}

fn frontmatter_visibility(metadata: &SkillMetadata) -> SkillVisibility {
    let Ok(content) = fs::read_to_string(&metadata.source_path) else {
        return SkillVisibility::Automatic;
    };
    parse_visibility(&content)
}

fn parse_visibility(content: &str) -> SkillVisibility {
    let Some(frontmatter) = yaml_frontmatter(content) else {
        return SkillVisibility::Automatic;
    };
    let Ok(parsed) = serde_yaml::from_str::<SkillFrontmatter>(frontmatter) else {
        return SkillVisibility::Automatic;
    };
    if parsed.disable_model_invocation {
        SkillVisibility::ManualOnly
    } else if parsed.paths.as_ref().is_some_and(yaml_paths_configured) {
        SkillVisibility::Conditional
    } else {
        SkillVisibility::Automatic
    }
}

fn yaml_paths_configured(value: &serde_yaml::Value) -> bool {
    match value {
        serde_yaml::Value::Null => false,
        serde_yaml::Value::String(value) => !value.trim().is_empty(),
        serde_yaml::Value::Sequence(values) => !values.is_empty(),
        _ => true,
    }
}

fn codex_manual_only(skill_dir: &Path) -> bool {
    let path = skill_dir.join("agents/openai.yaml");
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    serde_yaml::from_str::<OpenAiMetadata>(&content)
        .ok()
        .and_then(|data| data.policy)
        .and_then(|policy| policy.allow_implicit_invocation)
        .is_some_and(|allowed| !allowed)
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[derive(Default, Deserialize)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
    when_to_use: Option<String>,
    #[serde(default, rename = "disable-model-invocation")]
    disable_model_invocation: bool,
    paths: Option<serde_yaml::Value>,
    metadata: Option<serde_yaml::Value>,
}

#[derive(Deserialize)]
struct OpenAiMetadata {
    policy: Option<OpenAiPolicy>,
}

#[derive(Deserialize)]
struct OpenAiPolicy {
    allow_implicit_invocation: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn linked_project_scans_proma_native_skills_and_codex_business_skills() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let root = home.join(".proma");
        let native = root.join("agent-workspaces/pilot");
        let business = temp.path().join("business");
        fs::create_dir_all(&home).unwrap();
        write_skill(&native.join("skills"), "proma-only", "");
        write_skill(&native.join("skills-inactive"), "inactive", "");
        write_skill(&business.join(".codex/skills"), "codex-only", "");
        write_skill(&business.join("skills"), "business-data", "");
        fs::write(root.join("agent-workspaces.json"), serde_json::to_vec(&serde_json::json!({
            "version": 2, "workspaces": [{"id": "pilot", "slug": "pilot", "name": "Pilot",
                "projectRootPath": business}]
        })).unwrap()).unwrap();
        let estimates = estimate_project_with_home(&business, &home);
        let proma = estimates.iter().find(|estimate| estimate.agent == AgentKind::Proma).unwrap();
        assert_eq!(proma.skills.len(), 1);
        assert_eq!(proma.skills[0].name, "proma-only");
        assert!(proma.skills[0].path.starts_with(native.canonicalize().unwrap()));
        let codex = estimates.iter().find(|estimate| estimate.agent == AgentKind::Codex).unwrap();
        assert!(codex.skills.iter().any(|skill| skill.name == "codex-only"));
        let native_estimates = estimate_project_with_home(&native, &home);
        let native_proma = native_estimates.iter().find(|estimate| estimate.agent == AgentKind::Proma).unwrap();
        assert_eq!(native_proma.skills[0].path, proma.skills[0].path);
    }

    #[test]
    fn proma_metadata_uses_pi_invocation_rules_and_requires_description() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("SKILL.md");
        let context = DiscoveryContext { cwd: temp.path().into(), home: temp.path().into(), repository_root: None };
        fs::write(&file, "---\nname: alpha\ndescription: fixture\npaths: [src/**]\n---\n").unwrap();
        let metadata = read_metadata(&file).unwrap();
        assert!(PromaAdapter.is_enabled(temp.path(), &metadata, &context));
        assert_eq!(PromaAdapter.visibility(temp.path(), &metadata, &context), SkillVisibility::Automatic);
        fs::write(&file, "---\nname: alpha\ndescription: fixture\ndisable-model-invocation: true\n---\n").unwrap();
        let metadata = read_metadata(&file).unwrap();
        assert_eq!(PromaAdapter.visibility(temp.path(), &metadata, &context), SkillVisibility::ManualOnly);
        fs::write(&file, "---\nname: alpha\n---\nBody fallback").unwrap();
        let metadata = read_metadata(&file).unwrap();
        assert!(!PromaAdapter.is_enabled(temp.path(), &metadata, &context));
        fs::create_dir_all(temp.path().join("skills/alpha")).unwrap();
        fs::write(temp.path().join("skills/alpha/SKILL.md"), "---\nname: alpha\ndescription: fixture\n---\n").unwrap();
        assert!(PromaAdapter.roots(&context).is_empty());
    }
    use tempfile::tempdir;

    fn write_skill(root: &Path, name: &str, extra: &str) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Test\n{extra}---\nBody"),
        )
        .unwrap();
    }

    // Native-report contract fixtures; never launch an Agent from unit tests.
    fn grok_report_fixture(context: &DiscoveryContext, skills: serde_json::Value) -> AgentContextEstimate {
        let report = serde_json::json!({"grokVersion":"1.0.50", "cwd":context.cwd,
            "projectRoot":context.repository_root, "projectTrusted":true, "skills":skills});
        adapters::estimate_grok_catalog(context,
            grok_state::parse_json(&serde_json::to_vec(&report).unwrap()).unwrap())
    }

    #[test]
    fn manual_skills_are_discovered_but_not_counted() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("repo/subdir");
        fs::create_dir_all(project.join(".agents/skills")).unwrap();
        fs::create_dir_all(temp.path().join("repo/.git")).unwrap();
        write_skill(&project.join(".agents/skills"), "automatic", "");
        write_skill(
            &project.join(".agents/skills"),
            "manual",
            "disable-model-invocation: true\n",
        );
        let context = DiscoveryContext {
            cwd: project,
            home: temp.path().join("home"),
            repository_root: Some(temp.path().join("repo")),
        };
        let estimate = estimate_with_policy(&PiPolicy, &context);
        assert_eq!(estimate.discovered_count, 2);
        assert_eq!(estimate.model_visible_count, 1);
        assert_eq!(estimate.manual_only_count, 1);
    }

    #[test]
    fn codex_openai_policy_marks_manual_only() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("demo");
        write_skill(temp.path(), "demo", "");
        fs::create_dir_all(skill.join("agents")).unwrap();
        fs::write(
            skill.join("agents/openai.yaml"),
            "policy:\n  allow_implicit_invocation: false\n",
        )
        .unwrap();
        assert!(is_manual_skill(&skill));
    }

    #[test]
    fn invocation_extensions_are_only_applied_by_providers_that_support_them() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("manual");
        write_skill(
            temp.path(),
            "manual",
            "disable-model-invocation: true\nmetadata:\n  opencode:\n    autoinvoke: false\n",
        );
        let metadata = read_metadata(&skill.join("SKILL.md")).unwrap();
        let context = DiscoveryContext {
            cwd: temp.path().to_path_buf(),
            home: temp.path().join("home"),
            repository_root: None,
        };

        for policy in [
            &ClaudeCodePolicy as &dyn AgentSkillPolicy,
            &CursorPolicy,
            &OpenCodePolicy,
            &CopilotPolicy { roots: Vec::new() },
            &DroidPolicy,
            &PiPolicy,
            &OpenClawPolicy,
        ] {
            assert_eq!(
                policy.visibility(&skill, &metadata, &context),
                SkillVisibility::ManualOnly,
                "{} should honor its manual invocation metadata",
                policy.agent().label()
            );
        }
        let grok = grok_report_fixture(&context, serde_json::json!([{
            "name":"manual", "description":"Test", "source":{"type":"project","path":skill.join("SKILL.md")},
            "userInvocable":true
        }]));
        assert_eq!(grok.skills[0].visibility,
            if cfg!(unix) { SkillVisibility::ManualOnly } else { SkillVisibility::Unverified });
        assert_eq!(grok.model_visible_count, 0);
        assert_eq!(grok.manual_only_count, usize::from(cfg!(unix)));
        if !cfg!(unix) { assert!(grok.diagnostic.is_some()); }

        for policy in [
            &AntigravityPolicy as &dyn AgentSkillPolicy,
            &AmpPolicy,
            &HermesPolicy,
        ] {
            assert_eq!(
                policy.visibility(&skill, &metadata, &context),
                SkillVisibility::Automatic,
                "{} must not honor unsupported invocation metadata",
                policy.agent().label()
            );
        }
    }

    #[test]
    fn codex_token_estimate_is_not_capped_at_two_thousand() {
        let policy = CodexPolicy;
        let rendered = "x".repeat(8_001);

        assert_eq!(policy.estimate_tokens(&rendered), 2_001);
    }

    #[test]
    fn pi_uses_nearest_skill_when_names_collide() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        let cwd = repo.join("nested");
        fs::create_dir_all(repo.join(".git")).unwrap();
        write_skill(&cwd.join(".agents/skills"), "same", "");
        write_skill(&repo.join(".agents/skills"), "same", "");
        let context = DiscoveryContext {
            cwd: cwd.clone(),
            home: temp.path().join("home"),
            repository_root: Some(repo),
        };
        let estimate = estimate_with_policy(&PiPolicy, &context);
        assert_eq!(estimate.discovered_count, 2);
        assert_eq!(estimate.model_visible_count, 1);
        assert_eq!(estimate.skills.iter().filter(|skill| skill.visibility == SkillVisibility::Shadowed).count(), 1);
        assert!(estimate.skills[0].path.starts_with(cwd));
    }

    #[test]
    fn pi_only_uses_cwd_pi_directory_and_accepts_root_markdown() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        let cwd = repo.join("nested");
        fs::create_dir_all(cwd.join(".pi/skills")).unwrap();
        fs::create_dir_all(repo.join(".pi/skills")).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(
            cwd.join(".pi/skills/direct.md"),
            "---\nname: direct\ndescription: Direct skill\n---\n",
        )
        .unwrap();
        fs::write(
            cwd.join(".pi/skills/second.md"),
            "---\nname: second\ndescription: Second skill\n---\n",
        )
        .unwrap();
        assert_eq!(
            scanner::scan(
                &SkillRoot::new(cwd.join(".pi/skills"), SkillScope::Local).with_root_markdown(),
                scanner::ScanProfile::PiIgnored,
            )
            .len(),
            2
        );
        assert!(read_metadata(&cwd.join(".pi/skills/direct.md")).is_some());
        write_skill(&repo.join(".pi/skills"), "parent-pi", "");
        let context = DiscoveryContext {
            cwd,
            home: temp.path().join("home"),
            repository_root: Some(repo),
        };
        let estimate = estimate_with_policy(&PiPolicy, &context);
        assert_eq!(estimate.discovered_count, 2);
        assert!(estimate.skills.iter().any(|skill| skill.name == "direct"));
        assert!(estimate.skills.iter().any(|skill| skill.name == "second"));
    }

    #[test]
    fn pi_loads_settings_paths_for_other_agent_skill_roots() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        let cwd = repo.join("src");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&cwd).unwrap();

        write_skill(&home.join(".claude/skills"), "user-claude", "");
        write_skill(&home.join(".codex/skills"), "user-codex", "");
        write_skill(&repo.join(".claude/skills"), "project-claude", "");
        write_skill(&repo.join(".agents/skills"), "shared", "");

        let agent_dir = home.join(".pi/agent");
        fs::create_dir_all(&agent_dir).unwrap();
        fs::write(
            agent_dir.join("settings.json"),
            r#"{"skills":["~/.claude/skills","~/.codex/skills"]}"#,
        )
        .unwrap();
        fs::create_dir_all(cwd.join(".pi")).unwrap();
        fs::write(
            cwd.join(".pi/settings.json"),
            r#"{"skills":["../../.claude/skills"]}"#,
        )
        .unwrap();

        let context = DiscoveryContext {
            cwd,
            home,
            repository_root: Some(repo),
        };
        let estimate = estimate_with_policy(&PiPolicy, &context);
        for name in ["user-claude", "user-codex", "project-claude", "shared"] {
            assert!(
                estimate.skills.iter().any(|skill| skill.name == name),
                "Pi did not discover configured skill {name}"
            );
        }
    }

    #[test]
    fn pi_loads_static_skills_from_configured_packages() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        let package = project.join(".pi/npm/node_modules/example-pi-package");
        fs::create_dir_all(project.join(".pi")).unwrap();
        fs::create_dir_all(package.join("resources/release")).unwrap();
        fs::write(
            project.join(".pi/settings.json"),
            r#"{"packages":["npm:example-pi-package@1.0.0"]}"#,
        )
        .unwrap();
        fs::write(
            package.join("package.json"),
            r#"{"pi":{"skills":["resources/release"]}}"#,
        )
        .unwrap();
        fs::write(
            package.join("resources/release/SKILL.md"),
            "---\nname: package-release\ndescription: Release from package\n---\n",
        )
        .unwrap();

        let estimate = estimate_project_with_home(&project, &home)
            .into_iter()
            .find(|estimate| estimate.agent == AgentKind::Pi)
            .unwrap();
        assert!(
            estimate
                .skills
                .iter()
                .any(|skill| skill.name == "package-release")
        );
    }

    #[test]
    fn pi_requires_a_valid_description_and_uses_the_real_prompt_wrapper() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir_all(project.join(".pi/skills")).unwrap();
        fs::create_dir_all(project.join(".pi/skills/no-description")).unwrap();
        fs::write(
            project.join(".pi/skills/no-description/SKILL.md"),
            "---\nname: no-description\n---\nBody",
        )
        .unwrap();
        write_skill(&project.join(".pi/skills"), "valid", "");
        let context = DiscoveryContext {
            cwd: project,
            home: temp.path().join("home"),
            repository_root: None,
        };
        let estimate = estimate_with_policy(&PiPolicy, &context);
        assert!(
            estimate
                .skills
                .iter()
                .all(|skill| skill.name != "no-description")
        );
        let rendered = PiPolicy.render_visible_metadata(&estimate.skills);
        assert!(rendered.starts_with("\n\nThe following skills provide specialized instructions"));
        assert!(rendered.contains("<location>"));
        assert_eq!(estimate.estimated_tokens, approx_token_count(&rendered));
    }

    #[test]
    fn codex_config_parser_only_keeps_enabled_plugins() {
        let config =
            "[plugins.\"on@market\"]\nenabled = true\n\
             [plugins.\"off@market\"]\nenabled = false\n";
        assert_eq!(enabled_plugin_ids(config), vec!["on@market"]);
        let dotted_config = "[plugins]\n\
             \"on@market\".enabled = true\n\
             \"off@market\".enabled = false\n";
        assert_eq!(enabled_plugin_ids(dotted_config), vec!["on@market"]);
        assert!(enabled_plugin_ids("[plugins\ninvalid").is_empty());
        assert!(enabled_plugin_ids("[plugins]\ninvalid = true").is_empty());
    }

    #[test]
    fn codex_enabled_skill_config_selects_but_does_not_add_a_root() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        let external = temp.path().join("external");
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::create_dir_all(&project).unwrap();
        write_skill(&external, "configured-only", "");
        fs::write(
            home.join(".codex/config.toml"),
            format!(
                "[[skills.config]]\npath = {}\nenabled = true\n",
                toml::Value::String(external.to_string_lossy().into_owned())
            ),
        )
        .unwrap();

        let estimate = estimate_project_with_home(&project, &home)
            .into_iter()
            .find(|estimate| estimate.agent == AgentKind::Codex)
            .unwrap();
        assert!(
            !estimate
                .skills
                .iter()
                .any(|skill| skill.name == "configured-only")
        );
    }

    #[test]
    fn claude_uses_direct_children_body_fallback_and_keeps_same_names() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        let project_skill = project.join(".claude/skills/same");
        let user_skill = home.join(".claude/skills/same");
        let nested = project.join(".claude/skills/group/nested");
        fs::create_dir_all(&project_skill).unwrap();
        fs::create_dir_all(&user_skill).unwrap();
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            project_skill.join("SKILL.md"),
            "Project fallback description",
        )
        .unwrap();
        fs::write(user_skill.join("SKILL.md"), "User fallback description").unwrap();
        fs::write(nested.join("SKILL.md"), "Nested description").unwrap();

        let estimate = estimate_project_with_home(&project, &home)
            .into_iter()
            .find(|estimate| estimate.agent == AgentKind::ClaudeCode)
            .unwrap();
        assert_eq!(
            estimate
                .skills
                .iter()
                .filter(|skill| skill.name == "same")
                .count(),
            2
        );
        assert!(estimate.skills.iter().any(|skill| {
            skill.name == "same" && skill.description == "Project fallback description"
        }));
        assert!(
            !estimate
                .skills
                .iter()
                .any(|skill| skill.path == nested.join("SKILL.md"))
        );
    }

    #[test]
    fn grok_preserves_native_command_metadata_and_listing_fields() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        let commands = project.join(".grok/commands");
        fs::create_dir_all(&commands).unwrap();
        fs::write(commands.join("Release Notes.md"), "Prepare release notes").unwrap();
        let context = DiscoveryContext { cwd: project, home, repository_root: None };
        let estimate = grok_report_fixture(&context, serde_json::json!([{
            "name":"release-notes", "description":"Prepare release notes",
            "source":{"type":"project","path":commands.join("Release Notes.md")}, "userInvocable":true
        }]));
        let command = estimate
            .skills
            .iter()
            .find(|skill| skill.name == "release-notes")
            .unwrap();
        let rendered = GrokPolicy.render_visible_metadata(std::slice::from_ref(command));
        assert!(rendered.starts_with("<system-reminder>"));
        assert!(rendered.contains("The following skills are available for use:"));
        assert!(rendered.contains("Absolute path:"));
        assert!(rendered.contains(&command.path.display().to_string()));
        assert_eq!(estimate.diagnostic.is_none(), cfg!(unix));
        assert_eq!(estimate.skills.len(), 1);
    }

    #[test]
    fn grok_unknown_sources_keep_safe_metadata_without_effective_paths_or_counts() {
        let temp = tempdir().unwrap();
        let context = DiscoveryContext { cwd: temp.path().join("project"), home: temp.path().join("home"), repository_root: None };
        let known = temp.path().join("known/SKILL.md");
        fs::create_dir_all(known.parent().unwrap()).unwrap();
        fs::write(&known, "---\nname: known\ndescription: verified header\n---\n").unwrap();
        let hint = temp.path().join("not-a-scannable-skill");
        let estimate = grok_report_fixture(&context, serde_json::json!([
            {"name":"known","description":"supported","source":{"type":"user","path":known},"userInvocable":true},
            {"name":"plugin-row","description":"plugin description","source":{"type":"plugin","path":hint,"pluginName":"owned-plugin"},"userInvocable":true},
            {"name":"builtin-row","description":"builtin description","source":{"type":"builtin"},"userInvocable":false},
            {"name":"managed-row","description":"managed description","source":{"type":"managed"},"userInvocable":true,"disabled":true},
            {"name":"future-row","description":"future description","source":{"type":"future-provider","path":hint,"privatePayload":"synthetic-excluded"},"userInvocable":true,"invocableAs":"future-alias"}
        ]));
        assert_eq!(estimate.skills.len(), 1);
        assert_eq!(estimate.skills[0].path, known);
        assert_eq!(estimate.skills[0].visibility,
            if cfg!(unix) { SkillVisibility::Automatic } else { SkillVisibility::Unverified });
        let metadata = &estimate.diagnostic.as_ref().unwrap().metadata_only;
        assert_eq!(metadata.len(), 4);
        assert_eq!(metadata[0].source_path.as_deref(), Some(hint.as_path()));
        assert_eq!(metadata[0].source_identity.as_deref(), Some("owned-plugin"));
        assert!(metadata[1].source_path.is_none() && metadata[2].source_path.is_none());
        assert!(metadata[2].disabled);
        assert_eq!(metadata[3].source_type, "future-provider");
        assert_eq!(metadata[3].source_path.as_deref(), Some(hint.as_path()));
        assert_eq!(metadata[3].invocation_name.as_deref(), Some("future-alias"));
        let json = serde_json::to_value(&estimate).unwrap();
        for count in ["discovered_count", "model_visible_count", "manual_only_count", "name_only_count", "conditional_count", "estimated_tokens"] {
            assert!(json[count].is_null(), "{count}");
        }
        assert!(!json.to_string().contains("synthetic-excluded"));
        assert!(!hint.exists());
    }

    #[test]
    fn same_named_skills_keep_each_agents_actual_source() {
        let temp = tempdir().unwrap();
        let agents = crate::agents::AGENT_ICON_ORDER.iter()
            .filter_map(|info| AgentKind::from_id(info.id)).collect::<Vec<_>>();
        let skills = agents.iter().map(|agent| {
            let path = temp.path().join(agent.id()).join("SKILL.md");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, agent.id()).unwrap();
            EffectiveSkill {
                id: "pdf".into(), name: "pdf".into(), description: agent.label().into(), when_to_use: None,
                path, root_path: None, prompt_path: None, scope: SkillScope::Repository,
                user_invocable: true, visibility: SkillVisibility::Automatic, source: SkillSource::Filesystem,
            }
        }).collect::<Vec<_>>();
        let groups = group_effective_skills(agents.iter().copied().zip(skills.iter()));
        assert_eq!(groups.len(), agents.len(), "same names must not combine independent files");
        assert!(groups.iter().all(|group| group.entries().len() == 1));
        let mut alias = skills[0].clone();
        alias.name = "pdf-alias".into();
        let shared = group_effective_skills([(agents[0], &skills[0]), (agents[1], &alias)]);
        assert_eq!(shared.len(), 1, "one real file shared by Agents remains one row");
        assert_eq!(shared[0].entries().len(), 2);
    }

    #[test]
    fn same_named_builtins_and_plugins_keep_their_provider_identity() {
        let builtin = EffectiveSkill {
            id: "pdf".into(), name: "pdf".into(), description: String::new(), when_to_use: None,
            path: "/virtual/pdf".into(), root_path: None, prompt_path: None,
            scope: SkillScope::System, user_invocable: true, visibility: SkillVisibility::Automatic, source: SkillSource::Builtin,
        };
        let mut first_plugin = builtin.clone();
        first_plugin.source = SkillSource::Plugin { id: "first".into(), display_name: "PDF".into() };
        let mut second_plugin = first_plugin.clone();
        second_plugin.source = SkillSource::Plugin { id: "second".into(), display_name: "PDF".into() };
        let groups = group_effective_skills([
            (AgentKind::Codex, &builtin), (AgentKind::Proma, &builtin),
            (AgentKind::Codex, &first_plugin), (AgentKind::Codex, &second_plugin),
            (AgentKind::Codex, &first_plugin),
        ]);
        assert_eq!(groups.len(), 4);
        assert_eq!(groups.iter().filter(|group| group.entries().len() == 2).count(), 1);
    }

    #[test]
    fn groups_agent_specific_names_for_the_same_filesystem_skill() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        let commands = home.join(".claude/commands");
        fs::create_dir_all(&commands).unwrap();
        fs::create_dir_all(&project).unwrap();
        let command = commands.join("build_from_zero.md");
        fs::write(&command, "Build from zero").unwrap();

        let mut estimates = estimate_project_with_home(&project, &home);
        let context = DiscoveryContext { cwd: project, home, repository_root: None };
        *estimates.iter_mut().find(|estimate| estimate.agent == AgentKind::Grok).unwrap() =
            grok_report_fixture(&context, serde_json::json!([{
                "name":"build-from-zero", "description":"Build from zero",
                "source":{"type":"user","path":command}, "userInvocable":true
            }]));
        let groups = group_effective_skills(
            estimates
                .iter()
                .flat_map(|estimate| estimate.skills.iter().map(|skill| (estimate.agent, skill))),
        );
        let group = groups
            .iter()
            .find(|group| {
                group
                    .entries()
                    .iter()
                    .any(|(_, skill)| skill.path == command)
            })
            .unwrap();
        let names = group
            .entries()
            .iter()
            .map(|(_, skill)| skill.name.as_str())
            .collect::<HashSet<_>>();

        assert_eq!(group.name(), "build_from_zero");
        assert_eq!(names, HashSet::from(["build_from_zero", "build-from-zero"]));
        assert!(
            group
                .entries()
                .iter()
                .any(|(agent, _)| *agent == AgentKind::ClaudeCode)
        );
        assert!(
            group
                .entries()
                .iter()
                .any(|(agent, _)| *agent == AgentKind::Grok)
        );
    }

    #[test]
    fn grok_retains_native_collision_names_aliases_and_distinct_sources() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        let skills = project.join(".grok/skills");
        for directory in ["original", "copied"] {
            let path = skills.join(directory);
            fs::create_dir_all(&path).unwrap();
            fs::write(
                path.join("SKILL.md"),
                "---\nname: shared-name\ndescription: Shared\n---\n",
            )
            .unwrap();
        }

        let context = DiscoveryContext { cwd: project, home, repository_root: None };
        let estimate = grok_report_fixture(&context, serde_json::json!([
            {"name":"shared-name", "description":"Shared", "source":{"type":"project","path":skills.join("original/SKILL.md")}, "userInvocable":true},
            {"name":"copied", "description":"Shared", "source":{"type":"project","path":skills.join("copied/SKILL.md")}, "userInvocable":true,"collidesWith":"shared-name","invocableAs":"native-qualified-alias"}
        ]));
        let names = estimate
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<HashSet<_>>();
        assert!(names.contains("shared-name"));
        assert!(names.contains("copied"));
        assert_eq!(estimate.skills[1].id, "native-qualified-alias");
        assert_ne!(estimate.skills[0].path, estimate.skills[1].path);
    }

    #[test]
    fn codex_plugin_manifest_paths_and_source_are_preserved() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let codex_home = home.join(".codex");
        let plugin_root = codex_home.join("plugins/cache/market/deployer/1.0.0");
        fs::create_dir_all(plugin_root.join(".codex-plugin")).unwrap();
        fs::write(
            codex_home.join("config.toml"),
            "[plugins.\"deployer@market\"]\nenabled = true\n",
        )
        .unwrap();
        fs::write(
            plugin_root.join(".codex-plugin/plugin.json"),
            r#"{"name":"deployer","interface":{"displayName":"Deployer"},"paths":{"skills":["contributions/release"]}}"#,
        )
        .unwrap();
        write_skill(&plugin_root.join("contributions"), "release", "");
        write_skill(&plugin_root.join("skills"), "decoy", "");
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        let context = DiscoveryContext {
            cwd: repo.clone(),
            home,
            repository_root: Some(repo),
        };
        let estimate = estimate_with_policy(&CodexPolicy, &context);
        let plugin_skills = estimate.plugin_skills().collect::<Vec<_>>();
        assert_eq!(plugin_skills.len(), 1);
        assert_eq!(plugin_skills[0].name, "release");
        assert_eq!(
            plugin_skills[0].path,
            plugin_root.join("contributions/release/SKILL.md")
        );
        assert_eq!(
            plugin_skills[0].source,
            SkillSource::Plugin {
                id: "deployer@market".to_string(),
                display_name: "Deployer".to_string(),
            }
        );
    }

    #[test]
    fn codex_plugin_manifest_formats_control_skill_discovery() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let codex_home = home.join(".codex");
        let cache = codex_home.join("plugins/cache/market");
        let plugin_root = |name: &str| cache.join(name).join("1.0.0");
        fs::create_dir_all(&codex_home).unwrap();
        fs::write(
            codex_home.join("config.toml"),
            [
                "portable",
                "codex-legacy",
                "claude-legacy",
                "cursor-legacy",
                "portable-precedence",
                "legacy-precedence",
                "unrelated-root",
                "unsupported-root",
            ]
            .into_iter()
            .map(|name| format!("[plugins.\"{name}@market\"]\nenabled = true\n"))
            .collect::<String>(),
        )
        .unwrap();

        let portable = plugin_root("portable");
        fs::create_dir_all(&portable).unwrap();
        fs::write(
            portable.join("plugin.json"),
            format!(
                r#"{{"$schema":"{AGENT_PLUGIN_SCHEMA}","name":"portable","extensions":{{"com.openai":{{"interface":{{"displayName":"Portable Tools"}}}}}}}}"#
            ),
        )
        .unwrap();
        write_skill(&portable.join("skills"), "portable-direct", "");
        write_skill(&portable.join("skills/category"), "portable-nested", "");

        for (plugin, manifest) in [
            ("codex-legacy", ".codex-plugin/plugin.json"),
            ("claude-legacy", ".claude-plugin/plugin.json"),
            ("cursor-legacy", ".cursor-plugin/plugin.json"),
        ] {
            let root = plugin_root(plugin);
            fs::create_dir_all(root.join(Path::new(manifest).parent().unwrap())).unwrap();
            fs::write(root.join(manifest), format!(r#"{{"name":"{plugin}"}}"#)).unwrap();
            write_skill(
                &root.join("skills/category"),
                &format!("{plugin}-nested"),
                "",
            );
        }

        let portable_precedence = plugin_root("portable-precedence");
        fs::create_dir_all(portable_precedence.join(".codex-plugin")).unwrap();
        fs::write(
            portable_precedence.join("plugin.json"),
            format!(r#"{{"$schema":"{AGENT_PLUGIN_SCHEMA}","name":"portable-precedence"}}"#),
        )
        .unwrap();
        fs::write(
            portable_precedence.join(".codex-plugin/plugin.json"),
            r#"{"name":"legacy-decoy","skills":"legacy-skills"}"#,
        )
        .unwrap();
        write_skill(
            &portable_precedence.join("skills"),
            "portable-precedence-direct",
            "",
        );
        write_skill(
            &portable_precedence.join("legacy-skills"),
            "portable-precedence-decoy",
            "",
        );

        let legacy_precedence = plugin_root("legacy-precedence");
        fs::create_dir_all(legacy_precedence.join(".codex-plugin")).unwrap();
        fs::create_dir_all(legacy_precedence.join(".claude-plugin")).unwrap();
        fs::write(
            legacy_precedence.join(".codex-plugin/plugin.json"),
            r#"{"name":"codex-first","skills":"codex-skills"}"#,
        )
        .unwrap();
        fs::write(
            legacy_precedence.join(".claude-plugin/plugin.json"),
            r#"{"name":"claude-second","skills":"claude-skills"}"#,
        )
        .unwrap();
        write_skill(
            &legacy_precedence.join("codex-skills/category"),
            "legacy-precedence-codex",
            "",
        );
        write_skill(
            &legacy_precedence.join("claude-skills/category"),
            "legacy-precedence-claude",
            "",
        );

        let unrelated_root = plugin_root("unrelated-root");
        fs::create_dir_all(unrelated_root.join(".claude-plugin")).unwrap();
        fs::write(
            unrelated_root.join("plugin.json"),
            r#"{"name":"unrelated-package"}"#,
        )
        .unwrap();
        fs::write(
            unrelated_root.join(".claude-plugin/plugin.json"),
            r#"{"name":"unrelated-root"}"#,
        )
        .unwrap();
        write_skill(
            &unrelated_root.join("skills/category"),
            "unrelated-root-legacy",
            "",
        );

        let unsupported_root = plugin_root("unsupported-root");
        fs::create_dir_all(unsupported_root.join(".codex-plugin")).unwrap();
        fs::write(
            unsupported_root.join("plugin.json"),
            r#"{"$schema":"https://agent-plugins.org/schemas/2.0.0/plugin.schema.json","name":"unsupported"}"#,
        )
        .unwrap();
        fs::write(
            unsupported_root.join(".codex-plugin/plugin.json"),
            r#"{"name":"unsupported-fallback"}"#,
        )
        .unwrap();
        write_skill(
            &unsupported_root.join("skills/category"),
            "unsupported-root-decoy",
            "",
        );

        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        let context = DiscoveryContext {
            cwd: repo.clone(),
            home,
            repository_root: Some(repo),
        };
        let estimate = estimate_with_policy(&CodexPolicy, &context);
        let plugin_skills = estimate.plugin_skills().collect::<Vec<_>>();
        let names = plugin_skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<HashSet<_>>();

        for expected in [
            "portable-direct",
            "codex-legacy-nested",
            "claude-legacy-nested",
            "cursor-legacy-nested",
            "portable-precedence-direct",
            "legacy-precedence-codex",
            "unrelated-root-legacy",
        ] {
            assert!(names.contains(expected), "missing {expected}: {names:?}");
        }
        for excluded in [
            "portable-nested",
            "portable-precedence-decoy",
            "legacy-precedence-claude",
            "unsupported-root-decoy",
        ] {
            assert!(
                !names.contains(excluded),
                "unexpected {excluded}: {names:?}"
            );
        }
        assert!(plugin_skills.iter().any(|skill| {
            skill.name == "portable-direct"
                && skill.source
                    == (SkillSource::Plugin {
                        id: "portable@market".to_string(),
                        display_name: "Portable Tools".to_string(),
                    })
        }));
    }

    #[test]
    fn codex_reads_project_codex_skills_as_well_as_ancestor_agents_skills() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        let cwd = repo.join("nested");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        write_skill(&repo.join(".codex/skills"), "codex-local", "");
        write_skill(&repo.join(".agents/skills"), "shared", "");
        let context = DiscoveryContext {
            cwd,
            home: temp.path().join("home"),
            repository_root: Some(repo),
        };

        let estimate = estimate_with_policy(&CodexPolicy, &context);
        assert!(
            estimate
                .skills
                .iter()
                .any(|skill| skill.name == "codex-local")
        );
        assert!(estimate.skills.iter().any(|skill| skill.name == "shared"));
    }

    #[test]
    fn project_estimates_are_derived_from_the_selected_path() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let empty_project = temp.path().join("empty-project");
        let project_with_skill = temp.path().join("project-with-skill");
        fs::create_dir_all(&empty_project).unwrap();
        fs::create_dir_all(&project_with_skill).unwrap();
        write_skill(
            &project_with_skill.join(".agents/skills"),
            "selected-project-only",
            "",
        );

        let empty = estimate_project_with_home(&empty_project, &home);
        let selected = estimate_project_with_home(&project_with_skill, &home);

        for agent in [
            AgentKind::Codex,
            AgentKind::OpenCode,
            AgentKind::Pi,
        ] {
            let empty_count = empty
                .iter()
                .find(|estimate| estimate.agent == agent)
                .unwrap()
                .discovered_count;
            let selected_estimate = selected
                .iter()
                .find(|estimate| estimate.agent == agent)
                .unwrap();
            assert_eq!(selected_estimate.discovered_count, empty_count + 1);
            assert!(
                selected_estimate
                    .skills
                    .iter()
                    .any(|skill| skill.name == "selected-project-only")
            );
        }
        for estimates in [&empty, &selected] {
            let grok = estimates.iter().find(|estimate| estimate.agent == AgentKind::Grok).unwrap();
            assert!(grok.diagnostic.is_some() && grok.skills.is_empty());
            assert!(serde_json::to_value(grok).unwrap()["discovered_count"].is_null());
        }
        assert!(!empty.iter().any(|estimate| estimate.agent.is_global_only()));
        assert!(
            !selected
                .iter()
                .any(|estimate| estimate.agent.is_global_only())
        );
    }

    #[test]
    fn global_estimates_do_not_include_parent_project_roots() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        write_skill(&temp.path().join(".agents/skills"), "parent-project", "");
        write_skill(&home.join(".agents/skills"), "user-skill", "");

        let estimates = estimate_project_with_home(&home, &home);
        for agent in [
            AgentKind::Codex,
            AgentKind::OpenCode,
            AgentKind::Pi,
            AgentKind::Omp,
            AgentKind::DimAgent,
        ] {
            let estimate = estimates.iter().find(|item| item.agent == agent).unwrap();
            assert!(
                estimate
                    .skills
                    .iter()
                    .any(|skill| skill.name == "user-skill")
            );
            assert!(
                !estimate
                    .skills
                    .iter()
                    .any(|skill| skill.name == "parent-project")
            );
        }
        assert_eq!(
            estimates
                .iter()
                .filter(|estimate| estimate.agent.is_global_only())
                .count(),
            0
        );
    }

    #[test]
    fn compatible_agents_scan_both_agents_and_claude_roots() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        let home = temp.path().join("home");
        fs::create_dir_all(repo.join(".git")).unwrap();
        write_skill(&repo.join(".agents/skills"), "shared-agents", "");
        write_skill(&repo.join(".claude/skills"), "shared-claude", "");

        let context = DiscoveryContext {
            cwd: repo.clone(),
            home,
            repository_root: Some(repo),
        };
        for policy in [
            &CursorPolicy as &dyn AgentSkillPolicy,
            &CopilotPolicy { roots: copilot_roots_from(&context,
                &context.home.join(".copilot"), &serde_json::json!({}), None, true).unwrap() }
                as &dyn AgentSkillPolicy,
            &AmpPolicy as &dyn AgentSkillPolicy,
        ] {
            let estimate = estimate_with_policy(policy, &context);
            assert!(
                estimate
                    .skills
                    .iter()
                    .any(|skill| skill.name == "shared-agents")
            );
            assert!(
                estimate
                    .skills
                    .iter()
                    .any(|skill| skill.name == "shared-claude")
            );
        }
    }

    #[test]
    fn cursor_keeps_same_named_sources_and_shadows_compatible_roots() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        let home = temp.path().join("home");
        fs::create_dir_all(repo.join(".git")).unwrap();
        write_skill(&repo.join(".cursor/skills"), "same", "");
        write_skill(&repo.join(".agents/skills"), "same", "");
        write_skill(&repo.join(".claude/skills"), "same", "");

        let context = DiscoveryContext {
            cwd: repo.clone(),
            home,
            repository_root: Some(repo.clone()),
        };
        let estimate = estimate_with_policy(&CursorPolicy, &context);

        assert_eq!(estimate.discovered_count, 3);
        assert_eq!(estimate.model_visible_count, 1);
        assert_eq!(estimate.skills.iter().filter(|skill| skill.visibility == SkillVisibility::Shadowed).count(), 2);
        assert!(
            estimate.skills[0]
                .path
                .starts_with(repo.join(".cursor/skills"))
        );
    }

    #[test]
    fn every_effective_detector_has_a_matching_agent_badge() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let project_estimates = estimate_project_with_home(temp.path(), &home);
        let global_estimates = estimate_project_with_home(&home, &home);
        for estimate in project_estimates.into_iter().chain(global_estimates) {
            assert!(
                crate::agents::AGENT_ICON_ORDER
                    .iter()
                    .any(|agent| agent.id == estimate.agent.id()),
                "missing badge for {}",
                estimate.agent.id()
            );
        }
    }

    #[test]
    fn disabled_codex_plugin_does_not_appear_even_when_cached() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let codex_home = home.join(".codex");
        let plugin_root = codex_home.join("plugins/cache/market/off/1.0.0");
        fs::create_dir_all(plugin_root.join(".codex-plugin")).unwrap();
        fs::write(
            codex_home.join("config.toml"),
            "[plugins.\"off@market\"]\nenabled = false\n",
        )
        .unwrap();
        fs::write(
            plugin_root.join(".codex-plugin/plugin.json"),
            r#"{"name":"Off","skills":"skills"}"#,
        )
        .unwrap();
        write_skill(&plugin_root.join("skills"), "hidden", "");
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        let context = DiscoveryContext {
            cwd: repo.clone(),
            home,
            repository_root: Some(repo),
        };
        let estimate = estimate_with_policy(&CodexPolicy, &context);
        assert!(!estimate.plugin_skills().any(|skill| skill.name == "hidden"));
    }

    #[test]
    fn claude_plugin_manifest_string_path_and_source_are_preserved() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let plugin_root = home.join("plugins/deployer");
        fs::create_dir_all(plugin_root.join(".claude-plugin")).unwrap();
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        fs::write(
            home.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"deployer@market":true}}"#,
        )
        .unwrap();
        fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            serde_json::json!({
                "plugins": {
                    "deployer@market": [{
                        "scope": "user",
                        "installPath": plugin_root,
                    }],
                },
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            plugin_root.join(".claude-plugin/plugin.json"),
            r#"{"name":"Deployer UI","skills":"contributions/release"}"#,
        )
        .unwrap();
        write_skill(&plugin_root.join("contributions"), "release", "");
        write_skill(&plugin_root.join("skills"), "decoy", "");
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        let context = DiscoveryContext {
            cwd: repo.clone(),
            home,
            repository_root: Some(repo),
        };
        let estimate = estimate_with_policy(&ClaudeCodePolicy, &context);
        let plugin_skills = estimate.plugin_skills().collect::<Vec<_>>();
        assert_eq!(plugin_skills.len(), 1);
        assert_eq!(plugin_skills[0].name, "Deployer UI:release");
        assert_eq!(plugin_skills[0].source.plugin_id(), Some("deployer@market"));
        assert_eq!(
            plugin_skills[0].source.plugin_display_name(),
            Some("Deployer UI")
        );
    }

    #[test]
    fn claude_keeps_same_named_paths_and_honors_visibility_overrides() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        write_skill(&home.join(".claude/skills"), "same", "");
        write_skill(&repo.join(".claude/skills"), "same", "");
        write_skill(
            &repo.join(".claude/skills"),
            "manual-claude",
            "disable-model-invocation: true\n",
        );
        write_skill(&repo.join(".claude/skills"), "compact", "");
        fs::create_dir_all(repo.join(".claude")).unwrap();
        fs::write(
            repo.join(".claude/settings.local.json"),
            r#"{"skillOverrides":{"compact":"name-only"}}"#,
        )
        .unwrap();
        let context = DiscoveryContext {
            cwd: repo.clone(),
            home: home.clone(),
            repository_root: Some(repo),
        };
        let estimate = estimate_with_policy(&ClaudeCodePolicy, &context);
        assert_eq!(
            estimate
                .skills
                .iter()
                .filter(|skill| skill.name == "same")
                .count(),
            2
        );
        assert!(
            estimate
                .skills
                .iter()
                .filter(|skill| skill.name == "same")
                .any(|skill| skill.path.starts_with(&home))
        );
        assert!(
            estimate
                .skills
                .iter()
                .filter(|skill| skill.name == "same")
                .any(|skill| skill.path.starts_with(&context.cwd))
        );
        assert!(estimate.manual_only_count >= 1);
        assert!(estimate.name_only_count >= 1);
        assert_eq!(
            estimate
                .skills
                .iter()
                .find(|skill| skill.name == "manual-claude")
                .unwrap()
                .visibility,
            SkillVisibility::ManualOnly
        );
    }

    #[test]
    fn installed_agent_filter_excludes_compatible_but_absent_agents() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        write_skill(&home.join(".agents/skills"), "shared", "");
        write_skill(&home.join(".opencode/skills"), "leftover", "");
        let agents = crate::agents::AGENT_ICON_ORDER.iter().copied()
            .filter(|agent| matches!(agent.id, "codex" | "grok")).collect::<Vec<_>>();
        assert!(estimate_for_agents(&home, &home, &[]).is_empty());
        let estimates = estimate_for_agents(&home, &home, &agents);
        assert_eq!(estimates.iter().map(|estimate| estimate.agent).collect::<Vec<_>>(), [AgentKind::Codex, AgentKind::Grok]);
        assert_eq!(estimates[0].discovered_count, 1);
        assert_eq!(estimates[0].skills[0].name, "shared");
        assert!(estimates[1].diagnostic.is_some() && estimates[1].skills.is_empty());
        assert!(serde_json::to_value(&estimates[1]).unwrap()["discovered_count"].is_null());
    }

    #[test]
    fn opencode_estimates_only_skills_with_local_evidence() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap();

        for cwd in [&home, &repo] {
            let context = DiscoveryContext {
                cwd: cwd.clone(),
                home: home.clone(),
                repository_root: (cwd == &repo).then(|| repo.clone()),
            };
            let estimate = estimate_with_policy(&OpenCodePolicy, &context);
            assert_eq!(estimate.discovered_count, 0);
            assert_eq!(estimate.estimated_tokens, 0);
        }

        // Shared compatibility remains discoverable without an OpenCode install,
        // including a real skill that happens to use the former synthetic name.
        write_skill(&home.join(".agents/skills"), "customize-opencode", "");
        let context = DiscoveryContext {
            cwd: repo.clone(),
            home,
            repository_root: Some(repo),
        };
        let estimate = estimate_with_policy(&OpenCodePolicy, &context);
        assert_eq!(estimate.discovered_count, 1);
        assert_eq!(estimate.skills[0].name, "customize-opencode");
        assert_eq!(estimate.skills[0].source, SkillSource::Filesystem);
        assert!(estimate.skills[0].path.is_file());
        assert!(estimate.estimated_tokens > 0);
    }

    #[test]
    fn opencode_ignores_invocation_extension_but_honors_deny_permission() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        write_skill(
            &repo.join(".opencode/skills"),
            "still-visible",
            "disable-model-invocation: true\n",
        );
        write_skill(&repo.join(".opencode/skills"), "hidden", "");
        fs::write(
            repo.join("opencode.jsonc"),
            "{ // comment\n \"permission\": { \"skill\": { \"hidden\": \"deny\", }, },\n}",
        )
        .unwrap();
        let context = DiscoveryContext {
            cwd: repo.clone(),
            home,
            repository_root: Some(repo),
        };
        let estimate = estimate_with_policy(&OpenCodePolicy, &context);
        assert!(estimate.skills.iter().any(|skill| {
            skill.name == "still-visible" && skill.visibility == SkillVisibility::Automatic
        }));
        assert!(!estimate.skills.iter().any(|skill| skill.name == "hidden"));
    }

    #[test]
    fn opencode_uses_v2_ids_and_skills_array_sources() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(repo.join("team-skills")).unwrap();
        fs::write(
            repo.join("team-skills/release.md"),
            "---\nname: Release Guide\ndescription: Release workflow\nmetadata:\n  opencode:\n    autoinvoke: false\n---\nBody",
        )
        .unwrap();
        fs::write(
            repo.join("opencode.jsonc"),
            r#"{
                "skills": ["./team-skills"],
            }"#,
        )
        .unwrap();
        let context = DiscoveryContext {
            cwd: repo.clone(),
            home,
            repository_root: Some(repo),
        };
        let estimate = estimate_with_policy(&OpenCodePolicy, &context);
        let skill = estimate
            .skills
            .iter()
            .find(|skill| skill.name == "Release Guide")
            .unwrap();
        assert_eq!(skill.id, "release");
        assert_eq!(skill.scope, SkillScope::Repository);
        assert_eq!(skill.visibility, SkillVisibility::ManualOnly);
        assert!(
            !OpenCodePolicy
                .render_visible_metadata(&estimate.skills)
                .contains("<location>")
        );
    }

    #[test]
    fn opencode_v2_permissions_only_apply_skill_action_rules() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        write_skill(&repo.join(".opencode/skills"), "keep", "");
        fs::write(
            repo.join("opencode.json"),
            r#"{
                "permissions": [
                    {"action":"edit","resource":"keep","effect":"deny"},
                    {"action":"skill","resource":"keep","effect":"deny"}
                ]
            }"#,
        )
        .unwrap();
        let context = DiscoveryContext {
            cwd: repo.clone(),
            home,
            repository_root: Some(repo),
        };
        let estimate = estimate_with_policy(&OpenCodePolicy, &context);
        assert!(!estimate.skills.iter().any(|skill| skill.name == "keep"));
    }

    #[test]
    fn openclaw_preserves_native_model_only_and_manual_invocation_metadata() {
        let temp = tempdir().unwrap();
        let file = temp.path().join("SKILL.md");
        let context = DiscoveryContext {
            cwd: temp.path().into(), home: temp.path().into(), repository_root: None,
        };
        let policy = OpenClawSnapshotPolicy { snapshot: crate::agents::OpenClawSnapshot {
            config: serde_json::json!({}), state: temp.path().into(),
            routes: Vec::new(), effective_home: temp.path().into(),
        } };
        for (flags, user_invocable, visibility) in [
            ("", true, SkillVisibility::Automatic),
            ("user-invocable: false\n", false, SkillVisibility::Automatic),
            ("disable-model-invocation: true\n", true, SkillVisibility::ManualOnly),
            ("user-invocable: false\ndisable-model-invocation: true\n", false, SkillVisibility::Disabled),
            ("user-invocable: 'Off'\n", false, SkillVisibility::Automatic),
            ("user-invocable: unknown\n", true, SkillVisibility::Automatic),
        ] {
            fs::write(&file, format!("---\nname: probe\ndescription: fixture\n{flags}---\nBody")).unwrap();
            let metadata = read_metadata(&file).unwrap();
            assert_eq!(policy.user_invocable(temp.path(), &metadata, &context), user_invocable);
            assert_eq!(policy.visibility(temp.path(), &metadata, &context), visibility);
        }
    }

    #[test]
    fn openclaw_prefers_workspace_and_groups_enabled_plugin_skills() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let workspace = temp.path().join("workspace");
        let state = home.join(".openclaw");
        fs::create_dir_all(&workspace).unwrap();
        write_skill(&workspace.join("skills"), "same", "");
        write_skill(&home.join(".agents/skills"), "same", "");
        let plugin = state.join("extensions/demo");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(
            plugin.join("openclaw.plugin.json"),
            r#"{"id":"demo","name":"Demo Plugin","skills":["skills"]}"#,
        )
        .unwrap();
        write_skill(&plugin.join("skills"), "from-plugin", "");
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join("openclaw.json"),
            serde_json::to_vec(&serde_json::json!({"agents":{"defaults":{"workspace":workspace}},"plugins":{"enabled":true}})).unwrap(),
        )
        .unwrap();
        let context = DiscoveryContext {
            cwd: workspace.clone(),
            home,
            repository_root: None,
        };

        let estimate = estimate_with_policy(&OpenClawPolicy, &context);
        assert_eq!(
            estimate
                .skills
                .iter()
                .filter(|skill| skill.name == "same")
                .count(),
            2
        );
        assert_eq!(estimate.skills.iter().filter(|skill| skill.name == "same" && skill.visibility == SkillVisibility::Shadowed).count(), 1);
        assert!(
            estimate
                .skills
                .iter()
                .find(|skill| skill.name == "same")
                .unwrap()
                .path
                .starts_with(workspace)
        );
        let plugin_skill = estimate
            .plugin_skills()
            .find(|skill| skill.name == "from-plugin")
            .unwrap();
        assert_eq!(plugin_skill.source.plugin_id(), Some("demo"));
    }

    #[test]
    fn hermes_uses_home_and_explicit_external_dirs_only() {
        const FLAG: &str = "PLEAT_HERMES_ROOT_TEST";
        if std::env::var_os(FLAG).is_none() {
            let temp = tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let home = root.join("home");
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "effective_skills::tests::hermes_uses_home_and_explicit_external_dirs_only", "--nocapture"])
                .env_clear()
                .envs(std::env::var_os("SystemRoot").map(|value| ("SystemRoot", value)))
                .env(FLAG, &root).env("HOME", &home).env("USERPROFILE", &home)
                .env("HERMES_HOME", home.join(".hermes")).output().unwrap();
            assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            return;
        }
        let root = PathBuf::from(std::env::var_os(FLAG).unwrap());
        let home = root.join("home");
        let hermes = home.join(".hermes");
        let project = root.join("project");
        write_skill(&hermes.join("skills"), "local", "");
        write_skill(&hermes.join("shared"), "external", "");
        write_skill(&project.join(".agents/skills"), "not-implicit", "");
        fs::write(
            hermes.join("config.yaml"),
            "skills:\n  external_dirs:\n    - shared\n",
        )
        .unwrap();
        let context = DiscoveryContext {
            cwd: project,
            home,
            repository_root: None,
        };

        let estimate = estimate_with_policy(&HermesPolicy, &context);
        assert!(estimate.skills.iter().any(|skill| skill.name == "local"));
        assert!(estimate.skills.iter().any(|skill| skill.name == "external"));
        assert!(
            !estimate
                .skills
                .iter()
                .any(|skill| skill.name == "not-implicit")
        );
        assert!(
            !HermesPolicy
                .render_visible_metadata(&estimate.skills)
                .contains("SKILL.md")
        );
    }
}
