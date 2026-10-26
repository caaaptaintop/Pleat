use kitter::text::counted;
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use kitter::{
    InstallTarget, ProjectSkill, ProjectSkillInstallation, SkillGroup, SkillLibrary, SkillSummary,
    TriggerMode,
    adoption,
    effective_skills::{self, AgentContextEstimate, AgentKind, group_effective_skills},
    project, proma, source,
    tags::{TagId, TagState, load_tag_states, save_tag_states},
};
use serde::Serialize;

#[derive(Parser)]
#[command(
    name = "kitter",
    version,
    about = "Manage Agent Skills across projects"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List skills saved in Kitter
    List {
        /// Only show skills carrying this tag
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show one skill by name or ID
    Show {
        skill: String,
        #[arg(long)]
        json: bool,
    },
    /// Show or set trigger modes by unique name or id:<storage-name>
    Trigger {
        #[arg(required = true)]
        skills: Vec<String>,
        /// Omit to inspect without changing the library
        #[arg(long, value_enum)]
        mode: Option<TriggerModeArg>,
        #[arg(long)]
        json: bool,
    },
    /// List files in one skill
    Files { skill: String },
    /// Read one file from a skill
    Read { skill: String, path: PathBuf },
    /// Add skills from a source
    Add {
        #[command(subcommand)]
        source: AddSource,
    },
    /// Scan and adopt existing agent skill installations
    Adopt {
        /// Folders to scan; defaults to the home folder
        roots: Vec<PathBuf>,
        /// Adopt these exact source folders from the scan
        #[arg(long = "source", conflicts_with = "all")]
        sources: Vec<PathBuf>,
        /// Adopt every unambiguous candidate
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Remove one or more skills from Kitter and their managed installations
    Remove {
        #[arg(required = true)]
        skills: Vec<String>,
    },
    /// Install one or more skills into a project
    Install {
        #[arg(required = true)]
        skills: Vec<String>,
        #[arg(long)]
        project: PathBuf,
        #[arg(long, value_enum, required = true)]
        target: Vec<TargetArg>,
    },
    /// Remove selected skill installations from a project
    Uninstall {
        skills: Vec<String>,
        /// Remove these exact installation paths
        #[arg(long = "path")]
        paths: Vec<PathBuf>,
        #[arg(long)]
        project: PathBuf,
        /// Limit removal to these targets; defaults to every installed target
        #[arg(long, value_enum)]
        target: Vec<TargetArg>,
        /// Also remove external links or directly stored skill folders
        #[arg(long)]
        include_unmanaged: bool,
    },
    /// List remembered projects and native projects discovered from agents
    Projects {
        #[arg(long)]
        json: bool,
        /// Include archived and manually hidden projects
        #[arg(long)]
        all: bool,
    },
    /// Inspect direct and effective skills for a project
    Project {
        path: PathBuf,
        /// Limit effective discovery to one agent
        #[arg(long, value_enum)]
        agent: Option<AgentArg>,
        /// Select filesystem skills, plugin skills, or both
        #[arg(long, value_enum, default_value_t = ProjectViewArg::All)]
        view: ProjectViewArg,
        #[arg(long)]
        json: bool,
    },
    /// Inspect and manage one explicitly selected native Proma workspace
    Proma {
        #[command(subcommand)]
        action: PromaAction,
    },
    /// Update selected skills using their recorded sources
    Update {
        skills: Vec<String>,
        /// Update every skill with a managed source
        #[arg(long, conflicts_with = "skills")]
        all: bool,
    },
    /// Check every skill for updates
    Check {
        #[arg(long)]
        json: bool,
    },
    /// Manage skill groups
    Group {
        #[command(subcommand)]
        action: GroupAction,
    },
    /// Manage skill tags
    Tag {
        #[command(subcommand)]
        action: TagAction,
    },
    /// Show, switch or move the skill library
    Library {
        /// Switch to an existing nonempty skill library
        #[arg(long, value_name = "PATH")]
        set: Option<PathBuf>,
        /// Move the current library and keep its compatibility alias
        #[arg(long = "move", value_name = "PATH", conflicts_with = "set")]
        move_to: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum PromaAction {
    /// List registered native workspaces without initializing them
    Workspaces {
        #[arg(long)]
        json: bool,
    },
    /// Inspect active and inactive installations; runtime loading is not verified
    Status {
        #[arg(long)]
        workspace: String,
        #[arg(long)]
        json: bool,
    },
    /// Preview mirror updates without changing the workspace or library
    Plan {
        #[arg(long)]
        workspace: String,
        #[arg(required = true)]
        skills: Vec<String>,
    },
    /// Refresh only the specified workspace from registered library sources
    Sync {
        #[arg(long)]
        workspace: String,
        #[arg(required = true)]
        skills: Vec<String>,
    },
    /// Enable verified Proma built-in presets; user skills use install/uninstall
    Enable {
        #[arg(long)]
        workspace: String,
        #[arg(required = true)]
        skills: Vec<String>,
    },
    /// Disable verified Proma built-in presets while retaining their files
    Disable {
        #[arg(long)]
        workspace: String,
        #[arg(required = true)]
        skills: Vec<String>,
    },
    /// Recover an interrupted managed operation in the specified workspace
    Recover {
        #[arg(long)]
        workspace: String,
    },
}

#[derive(Subcommand)]
enum AddSource {
    /// Recursively discover skills below a local folder
    Local {
        path: PathBuf,
        /// Import only these discovered skill names; defaults to all
        #[arg(long = "skill")]
        skills: Vec<String>,
        #[arg(long)]
        group: Option<String>,
    },
    /// Discover skills from a skills.sh or GitHub source
    Npx {
        repository: String,
        /// Import only these discovered skill names; defaults to all
        #[arg(long = "skill")]
        skills: Vec<String>,
        #[arg(long)]
        group: Option<String>,
    },
    /// Discover skills from a Claude plugin
    Claude {
        plugin: String,
        /// Import only these discovered skill names; defaults to all
        #[arg(long = "skill")]
        skills: Vec<String>,
        #[arg(long)]
        group: Option<String>,
    },
}

#[derive(Subcommand)]
enum GroupAction {
    /// List groups and their skill counts
    List {
        #[arg(long)]
        json: bool,
    },
    /// Create a group
    Create { name: String },
    /// Rename a group
    Rename { group: String, name: String },
    /// Delete a group
    Delete {
        group: String,
        /// Delete skills in the group instead of leaving them ungrouped
        #[arg(long)]
        delete_skills: bool,
    },
    /// Move skills into a group
    Assign {
        group: String,
        #[arg(required = true)]
        skills: Vec<String>,
    },
    /// Remove skills from their groups
    Clear {
        #[arg(required = true)]
        skills: Vec<String>,
    },
}

#[derive(Subcommand)]
enum TagAction {
    /// List tags and assignment counts
    List {
        #[arg(long)]
        json: bool,
    },
    /// Create a root or child tag
    Create {
        name: String,
        /// Parent tag name or ID for a second-level tag
        #[arg(long, value_name = "TAG")]
        parent: Option<String>,
    },
    /// Rename a tag
    Rename {
        /// Tag name or ID
        tag: String,
        name: String,
    },
    /// Delete a tag and its child tags
    Delete {
        /// Tag name or ID
        tag: String,
    },
    /// Assign a tag to one or more skills
    Assign {
        /// Tag name or ID
        tag: String,
        #[arg(required = true)]
        items: Vec<String>,
    },
    /// Remove a tag from one or more skills
    Unassign {
        /// Tag name or ID
        tag: String,
        #[arg(required = true)]
        items: Vec<String>,
    },
    /// Reorder a tag among its siblings
    Move {
        /// Tag name or ID
        tag: String,
        #[arg(
            long,
            value_name = "TAG",
            conflicts_with = "after",
            required_unless_present = "after"
        )]
        before: Option<String>,
        #[arg(
            long,
            value_name = "TAG",
            conflicts_with = "before",
            required_unless_present = "before"
        )]
        after: Option<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum TriggerModeArg {
    FollowSkill,
    Manual,
    Automatic,
}

impl From<TriggerModeArg> for TriggerMode {
    fn from(value: TriggerModeArg) -> Self {
        match value {
            TriggerModeArg::FollowSkill => Self::FollowSkill,
            TriggerModeArg::Manual => Self::Manual,
            TriggerModeArg::Automatic => Self::Automatic,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum TargetArg {
    Universal,
    Codex,
    Claude,
    Cursor,
    Opencode,
    Pi,
    Proma,
    Omp,
    Grok,
    Antigravity,
    Droid,
    Copilot,
    Trae,
    Workbuddy,
    Openclaw,
    Hermes,
}

impl From<TargetArg> for InstallTarget {
    fn from(value: TargetArg) -> Self {
        match value {
            TargetArg::Universal => Self::Universal,
            TargetArg::Codex => Self::Codex,
            TargetArg::Claude => Self::ClaudeCode,
            TargetArg::Cursor => Self::Cursor,
            TargetArg::Opencode => Self::OpenCode,
            TargetArg::Pi => Self::Pi,
            TargetArg::Proma => Self::Proma,
            TargetArg::Omp => Self::Omp,
            TargetArg::Grok => Self::Grok,
            TargetArg::Antigravity => Self::Antigravity,
            TargetArg::Droid => Self::Droid,
            TargetArg::Copilot => Self::Copilot,
            TargetArg::Trae => Self::Trae,
            TargetArg::Workbuddy => Self::WorkBuddy,
            TargetArg::Openclaw => Self::OpenClaw,
            TargetArg::Hermes => Self::Hermes,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum AgentArg {
    Dimagent,
    Codex,
    Claude,
    Cursor,
    Opencode,
    Copilot,
    Antigravity,
    Amp,
    Droid,
    Pi,
    Proma,
    Omp,
    Grok,
    Openclaw,
    Hermes,
    Trae,
    Workbuddy,
}

impl From<AgentArg> for AgentKind {
    fn from(value: AgentArg) -> Self {
        match value {
            AgentArg::Codex => Self::Codex,
            AgentArg::Claude => Self::ClaudeCode,
            AgentArg::Cursor => Self::Cursor,
            AgentArg::Opencode => Self::OpenCode,
            AgentArg::Copilot => Self::Copilot,
            AgentArg::Antigravity => Self::Antigravity,
            AgentArg::Amp => Self::Amp,
            AgentArg::Droid => Self::Droid,
            AgentArg::Pi => Self::Pi,
            AgentArg::Proma => Self::Proma,
            AgentArg::Omp => Self::Omp,
            AgentArg::Dimagent => Self::DimAgent,
            AgentArg::Grok => Self::Grok,
            AgentArg::Openclaw => Self::OpenClaw,
            AgentArg::Hermes => Self::Hermes,
            AgentArg::Trae => Self::Trae,
            AgentArg::Workbuddy => Self::WorkBuddy,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum ProjectViewArg {
    #[default]
    All,
    Skills,
    Plugins,
}

#[derive(Serialize)]
struct SkillOutput {
    id: String,
    name: String,
    description: String,
    origin: kitter::SkillOrigin,
    path: PathBuf,
    group: Option<String>,
    tags: Vec<String>,
    installed_projects: usize,
    manual_only: bool,
    update_available: bool,
}

#[derive(Serialize)]
struct AdoptionOutput {
    name: String,
    source: PathBuf,
    origin: kitter::SkillOrigin,
    references: Vec<kitter::adoption::SkillReference>,
    issue: Option<String>,
    conflict: bool,
    already_managed: bool,
    selectable: bool,
}

#[derive(Serialize)]
struct ProjectOutput {
    path: PathBuf,
    direct_installations: Vec<ProjectSkill>,
    agents: Vec<AgentContextEstimate>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    native_invocations: Vec<NativeInvocationOutput>,
}

#[derive(Serialize)]
struct NativeInvocationOutput {
    agent: &'static str,
    native_id: String,
    source_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    invocation: Option<kitter::GrokInvocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct GroupOutput {
    id: String,
    name: String,
    skills: usize,
}

#[derive(Serialize)]
struct TagOutput {
    id: String,
    name: String,
    parent: Option<String>,
    assignments: usize,
}

fn main() {
    kitter::run_native_adaptation_reader_if_requested();
    if let Some(code) = kitter::project_discovery::run_metadata_worker() {
        std::process::exit(code);
    }
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let command = match Cli::parse().command {
        Command::Proma { action } => return manage_proma(action),
        Command::Trigger {
            skills,
            mode: None,
            json,
        } => {
            let (mut library, _snapshot) = open_library_snapshot(&kitter::config::app_data_dir())?;
            return manage_trigger(&mut library, &skills, None, json);
        }
        command => command,
    };
    let mut library = SkillLibrary::open()?;
    match command {
        Command::List { tag, json } => list_skills(&library, tag.as_deref(), json),
        Command::Show { skill, json } => show_skill(&library, &skill, json),
        Command::Trigger { skills, mode, json } => {
            manage_trigger(&mut library, &skills, mode.map(Into::into), json)
        }
        Command::Files { skill } => {
            let skill = library.resolve_skill(&skill)?;
            for file in library.files_by_storage(&skill.record.storage_name)? {
                println!("{}", file.display());
            }
            Ok(())
        }
        Command::Read { skill, path } => {
            let skill = library.resolve_skill(&skill)?;
            print!(
                "{}",
                library.read_file_by_storage(&skill.record.storage_name, &path)?
            );
            Ok(())
        }
        Command::Add { source: add } => add_skills(&mut library, add),
        Command::Adopt {
            roots,
            sources,
            all,
            json,
        } => adopt_skills(&mut library, roots, sources, all, json),
        Command::Remove { skills } => remove_skills(&mut library, &skills),
        Command::Install {
            skills,
            project,
            target,
        } => install_skills(&library, &skills, &project, target),
        Command::Uninstall {
            skills,
            paths,
            project,
            target,
            include_unmanaged,
        } => uninstall_skills(
            &library,
            &skills,
            &paths,
            &project,
            target,
            include_unmanaged,
        ),
        Command::Project {
            path,
            agent,
            view,
            json,
        } => inspect_project(&library, &path, agent, view, json),
        Command::Proma { .. } => unreachable!("Proma commands are handled before library opening"),
        Command::Projects { json, all } => {
            let discovered = kitter::project_discovery::discover_agent_projects()?;
            let projects = library.config.project_entries(&discovered)
                .into_iter().filter(|entry| all || entry.is_active()).collect::<Vec<_>>();
            if json {
                println!("{}", serde_json::to_string_pretty(&projects)?);
            } else {
                for entry in projects {
                    println!("{}  {}", entry.name, entry.path.display());
                }
            }
            Ok(())
        },
        Command::Update { skills, all } => update_skills(&mut library, &skills, all),
        Command::Check { json } => {
            let count = source::check_updates(&mut library)?;
            if json {
                println!("{{\"updates\":{count}}}");
            } else {
                println!("{} can be updated", counted(count, "skill", "skills"));
            }
            Ok(())
        }
        Command::Group { action } => manage_groups(&mut library, action),
        Command::Tag { action } => manage_tags(&library, action),
        Command::Library { set, move_to } => manage_library(&mut library, set, move_to),
    }
}

fn list_skills(library: &SkillLibrary, tag: Option<&str>, json: bool) -> Result<()> {
    let (skill_tags, _) = load_tag_states();
    let tag_id = tag.map(|tag| resolve_tag(&skill_tags, tag)).transpose()?;
    let groups = library
        .groups()
        .into_iter()
        .map(|group| (group.id, group.name))
        .collect::<BTreeMap<_, _>>();
    let skills = library
        .list()?
        .into_iter()
        .filter(|skill| {
            tag_id.is_none_or(|tag| skill_tags.matches_filter(&skill.record.storage_name, tag))
        })
        .map(|skill| skill_output(skill, &groups, &skill_tags))
        .collect::<Vec<_>>();
    let duplicate_names = skills.iter().fold(BTreeMap::new(), |mut counts, skill| {
        *counts.entry(skill.name.clone()).or_insert(0usize) += 1;
        counts
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&skills)?);
    } else if skills.is_empty() {
        println!("No skills found");
    } else {
        for skill in skills {
            let group = skill
                .group
                .as_deref()
                .map(|group| format!("  [{group}]"))
                .unwrap_or_default();
            let tags = if skill.tags.is_empty() {
                String::new()
            } else {
                format!(
                    "  {}",
                    skill
                        .tags
                        .iter()
                        .map(|tag| format!("#{tag}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            };
            let id = if duplicate_names
                .get(&skill.name)
                .copied()
                .unwrap_or_default()
                > 1
            {
                format!("  {}", skill.id)
            } else {
                String::new()
            };
            println!(
                "{:<28} {}{}{}{}",
                skill.name, skill.description, group, tags, id
            );
        }
    }
    Ok(())
}

fn show_skill(library: &SkillLibrary, selector: &str, json: bool) -> Result<()> {
    let skill = library.resolve_skill(selector)?;
    let (skill_tags, _) = load_tag_states();
    let groups = library
        .groups()
        .into_iter()
        .map(|group| (group.id, group.name))
        .collect::<BTreeMap<_, _>>();
    let skill = skill_output(skill, &groups, &skill_tags);
    if json {
        println!("{}", serde_json::to_string_pretty(&skill)?);
    } else {
        println!("{}", skill.name);
        if skill.id != skill.name {
            println!("ID: {}", skill.id);
        }
        println!("{}", skill.description);
        println!("Source: {}", skill.origin.label());
        println!("Path: {}", skill.path.display());
        if let Some(group) = skill.group {
            println!("Group: {group}");
        }
        if !skill.tags.is_empty() {
            println!("Tags: {}", skill.tags.join(", "));
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct TriggerOutput {
    selector: String,
    name: Option<String>,
    storage_name: Option<String>,
    mode: Option<TriggerMode>,
    changed: bool,
    error: Option<String>,
}

fn manage_trigger(
    library: &mut SkillLibrary,
    selectors: &[String],
    mode: Option<TriggerMode>,
    json: bool,
) -> Result<()> {
    let mut output = Vec::new();
    for selector in selectors {
        let mut item = TriggerOutput {
            selector: selector.clone(),
            name: None,
            storage_name: None,
            mode: None,
            changed: false,
            error: None,
        };
        match library.resolve_skill(selector) {
            Ok(skill) => {
                let storage = &skill.record.storage_name;
                item.name = Some(skill.record.name.clone());
                item.storage_name = Some(storage.clone());
                if let Some(mode) = mode {
                    match library.apply_trigger_mode_by_storage(storage, mode) {
                        Ok(changed) => item.changed = changed,
                        Err(error) => item.error = Some(format!("{error:#}")),
                    }
                }
                item.mode = Some(library.trigger_mode_by_storage(storage));
            }
            Err(error) => item.error = Some(format!("{error:#}")),
        }
        output.push(item);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        for item in &output {
            let storage = item.storage_name.as_deref().unwrap_or(&item.selector);
            if let Some(error) = &item.error {
                println!("{storage}: error: {error}");
            } else {
                let mode = match item.mode.expect("resolved skill has a trigger mode") {
                    TriggerMode::FollowSkill => "follow-skill",
                    TriggerMode::Manual => "manual",
                    TriggerMode::Automatic => "automatic",
                };
                let status = if item.changed { "changed" } else { "unchanged" };
                println!("{storage}: {mode} ({status})");
            }
        }
    }
    let failures = output.iter().filter(|item| item.error.is_some()).count();
    if failures > 0 {
        bail!("{failures} trigger operation(s) failed; see per-skill results");
    }
    Ok(())
}

fn skill_output(
    skill: SkillSummary,
    groups: &BTreeMap<String, String>,
    tags: &TagState,
) -> SkillOutput {
    let tag_names = tags
        .assigned_tags(&skill.record.storage_name)
        .into_iter()
        .map(|tag| tag.name.clone())
        .collect();
    SkillOutput {
        id: format!("id:{}", skill.record.storage_name),
        name: skill.record.name.clone(),
        description: skill.record.description.clone(),
        origin: skill.record.origin.clone(),
        path: skill.path,
        group: skill
            .record
            .group_id
            .as_ref()
            .and_then(|id| groups.get(id))
            .cloned(),
        tags: tag_names,
        installed_projects: skill.installed_projects,
        manual_only: skill.manual_only,
        update_available: skill.record.update_available,
    }
}

fn add_skills(library: &mut SkillLibrary, add: AddSource) -> Result<()> {
    let (scan, selected, group) = match add {
        AddSource::Local {
            path,
            skills,
            group,
        } => {
            let path = path
                .canonicalize()
                .with_context(|| format!("找不到来源目录：{}", path.display()))?;
            (source::scan_local(&path)?, skills, group)
        }
        AddSource::Npx {
            repository,
            skills,
            group,
        } => (source::scan_npx(&repository)?, skills, group),
        AddSource::Claude {
            plugin,
            skills,
            group,
        } => (source::scan_claude(&plugin)?, skills, group),
    };
    let available = scan
        .skills()
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<BTreeSet<_>>();
    let selected = if selected.is_empty() {
        available.iter().cloned().collect::<HashSet<_>>()
    } else {
        let missing = selected
            .iter()
            .filter(|skill| !available.contains(*skill))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            bail!("没有从来源中找到：{}", missing.join(", "));
        }
        selected.into_iter().collect()
    };
    let summary = scan.import_selected(library, &selected, group.as_deref())?;
    println!(
        "Added {}, skipped {} already added",
        counted(summary.added, "skill", "skills"),
        counted(summary.skipped, "skill", "skills")
    );
    Ok(())
}

fn adopt_skills(
    library: &mut SkillLibrary,
    roots: Vec<PathBuf>,
    sources: Vec<PathBuf>,
    all: bool,
    json: bool,
) -> Result<()> {
    let home = dirs::home_dir().context("找不到用户目录")?;
    let roots = if roots.is_empty() {
        vec![home.clone()]
    } else {
        roots
    };
    let managed = library.list()?;
    let scan = adoption::scan_roots(
        &home,
        &roots,
        &library.config.library_dir,
        &managed,
        &AtomicBool::new(false),
    )?;
    let output = scan
        .candidates
        .iter()
        .map(|candidate| AdoptionOutput {
            name: candidate.name.clone(),
            source: candidate.source.clone(),
            origin: candidate.origin.clone(),
            references: candidate.references.clone(),
            issue: candidate.issue.clone(),
            conflict: scan.has_conflict(&candidate.identity()),
            already_managed: candidate.existing_storage.is_some(),
            selectable: scan.selectable_ids().contains(&candidate.id),
        })
        .collect::<Vec<_>>();

    if !all && sources.is_empty() {
        if json {
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else if output.is_empty() {
            println!("No existing skill installations found");
        } else {
            for candidate in output {
                let status = if candidate.issue.is_some() {
                    "unavailable"
                } else if candidate.conflict {
                    "choose-source"
                } else if candidate.already_managed {
                    "managed"
                } else {
                    "ready"
                };
                println!(
                    "{:<24} {:<14} {}",
                    candidate.name,
                    status,
                    candidate.source.display()
                );
            }
            println!("Run with --all or one or more --source paths to adopt");
        }
        return Ok(());
    }

    let mut selected = if all {
        scan.default_selection()
    } else {
        HashSet::new()
    };
    for source in sources {
        let source = source.canonicalize().unwrap_or(source);
        let candidate = scan
            .candidates
            .iter()
            .find(|candidate| candidate.source == source)
            .with_context(|| format!("扫描结果中没有这个来源：{}", source.display()))?;
        if let Some(issue) = &candidate.issue {
            bail!("无法托管 {}：{issue}", candidate.source.display());
        }
        scan.select(&mut selected, &candidate.id);
    }
    if selected.is_empty() {
        bail!("没有可托管的 skill；冲突来源需要使用 --source 明确选择");
    }

    let mut adopted = 0usize;
    let mut failures = Vec::new();
    for candidate in scan
        .candidates
        .iter()
        .filter(|candidate| selected.contains(&candidate.id))
    {
        let result = scan
            .variants(candidate)
            .try_for_each(|variant| variant.verify())
            .and_then(|()| library.adopt(candidate, &scan.references_for(candidate)));
        match result {
            Ok(_) => adopted += 1,
            Err(error) => failures.push(format!("{}：{error:#}", candidate.name)),
        }
    }
    finish_batch("Adopted", "skill", adopted, failures)
}

fn remove_skills(library: &mut SkillLibrary, selectors: &[String]) -> Result<()> {
    let skills = resolve_skills(library, selectors)?;
    let mut removed = 0usize;
    let mut failures = Vec::new();
    for skill in skills {
        match library.remove_by_storage(&skill.record.storage_name) {
            Ok(()) => removed += 1,
            Err(error) => failures.push(format!("{}：{error:#}", skill.record.name)),
        }
    }
    finish_batch("Removed", "skill", removed, failures)
}

fn install_skills(
    library: &SkillLibrary,
    selectors: &[String],
    project_path: &Path,
    targets: Vec<TargetArg>,
) -> Result<()> {
    let project_path = canonical_project(project_path)?;
    let skills = resolve_skills(library, selectors)?;
    let targets = targets.into_iter().map(Into::into).collect::<Vec<_>>();
    let mut installed = 0usize;
    let mut failures = Vec::new();
    for skill in skills {
        match project::install_from_path(&project_path, &skill.path, &skill.record.name, &targets) {
            Ok(()) => installed += 1,
            Err(error) => failures.push(format!("{}：{error:#}", skill.record.name)),
        }
    }
    finish_batch("Installed", "skill", installed, failures)
}

fn uninstall_skills(
    library: &SkillLibrary,
    selectors: &[String],
    paths: &[PathBuf],
    project_path: &Path,
    targets: Vec<TargetArg>,
    include_unmanaged: bool,
) -> Result<()> {
    if selectors.is_empty() && paths.is_empty() {
        bail!("请指定至少一个 skill 或 --path 安装位置");
    }
    let project_path = canonical_project(project_path)?;
    let installed = project::list(&project_path, &library.config.library_dir)?;
    let library_skills = library.list()?;
    let targets = targets
        .into_iter()
        .map(InstallTarget::from)
        .collect::<HashSet<_>>();
    let mut selected = Vec::<ProjectSkillInstallation>::new();
    for requested in paths {
        let requested = if requested.is_absolute() {
            requested.clone()
        } else {
            project_path.join(requested)
        };
        let requested_key = project::installation_key(&requested);
        let matching = installed
            .iter()
            .flat_map(|skill| skill.installations.iter())
            .filter(|installation| targets.is_empty() || targets.contains(&installation.target))
            .find(|installation| project::installation_key(&installation.path) == requested_key)
            .cloned()
            .with_context(|| format!("这个项目中没有安装位置：{}", requested.display()))?;
        selected.push(matching);
    }
    for selector in selectors {
        let library_matches = matching_library_skills(&library_skills, selector);
        if library_matches.len() > 1 {
            bail!("存在多个名为 {selector} 的 skill，请使用 list 中的 ID");
        }
        let matching = installed
            .iter()
            .filter(|skill| {
                library_matches
                    .first()
                    .map(|library_skill| skill.name == library_skill.record.name)
                    .unwrap_or_else(|| skill.name == *selector)
            })
            .flat_map(|skill| skill.installations.iter())
            .filter(|installation| targets.is_empty() || targets.contains(&installation.target))
            .filter(|installation| {
                library_matches
                    .first()
                    .is_none_or(|skill| same_file(&installation.path, &skill.path)
                        || (installation.target == InstallTarget::Proma
                            && proma::is_managed(&installation.path, &skill.path)))
            })
            .cloned()
            .collect::<Vec<_>>();
        if matching.is_empty() {
            bail!("这个项目中没有匹配的安装：{selector}");
        }
        if matching.iter().any(|installation| installation.target == InstallTarget::Proma)
            && proma::project_workspaces(&project_path)?.len() > 1
        {
            bail!("项目关联多个 Proma 工作区，请指定一个原生工作区目录或使用 --path 精确安装位置");
        }
        selected.extend(matching);
    }
    let unsafe_paths = selected
        .iter()
        .filter(|installation| !installation.managed)
        .map(|installation| installation.path.display().to_string())
        .collect::<Vec<_>>();
    if !unsafe_paths.is_empty() && !include_unmanaged {
        bail!(
            "所选位置包含外部链接或直接保存的文件；确认后使用 --include-unmanaged：{}",
            unsafe_paths.join(", ")
        );
    }
    let report = project::remove_project_skills_at(&project_path, &selected);
    finish_batch("Removed", "installation", report.removed, report.failures)
}

fn inspect_project(
    library: &SkillLibrary,
    path: &Path,
    agent: Option<AgentArg>,
    view: ProjectViewArg,
    json: bool,
) -> Result<()> {
    let path = canonical_project(path)?;
    let direct = project::list(&path, &library.config.library_dir)?;
    let selected_agent = agent.map(AgentKind::from);
    let snapshots = match selected_agent {
        Some(agent) => effective_skills::estimate_project_for_explicit_agent(&path, agent),
        None => effective_skills::estimate_project(&path),
    };
    let mut estimates = snapshots
        .into_iter()
        .filter(|estimate| selected_agent.is_none_or(|agent| estimate.agent == agent))
        .collect::<Vec<_>>();
    for estimate in &mut estimates {
        estimate.skills.retain(|skill| match view {
            ProjectViewArg::All => true,
            ProjectViewArg::Skills => !skill.source.is_plugin(),
            ProjectViewArg::Plugins => skill.source.is_plugin(),
        });
    }
    if json {
        let native_invocations = estimates.iter().filter(|estimate| estimate.agent == AgentKind::Grok)
            .flat_map(|estimate| &estimate.skills)
            .filter(|skill| skill.user_invocable
                && matches!(skill.source, effective_skills::SkillSource::Filesystem)
                && matches!(skill.visibility, effective_skills::SkillVisibility::Automatic | effective_skills::SkillVisibility::ManualOnly))
            .map(|skill| {
                let result = if skill.id.contains(':') {
                    kitter::grok_invocation(&path, skill, None)
                } else { Err(anyhow::anyhow!("宿主未提供可核验的技能调用标识")) };
                let (invocation, error) = match result {
                    Ok(invocation) => (invocation, None),
                    Err(error) => (None, Some(error.to_string())),
                };
                NativeInvocationOutput { agent: "grok", native_id: skill.id.clone(),
                    source_path: skill.path.clone(), invocation, error }
            }).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&ProjectOutput {
                path,
                direct_installations: direct,
                agents: estimates,
                native_invocations,
            })?
        );
        return Ok(());
    }

    println!("Project: {}", path.display());
    println!("Direct installations: {}", direct.len());
    for skill in &direct {
        let targets = skill
            .installations
            .iter()
            .map(|installation| {
                if let Some(state) = &installation.state {
                    format!("{:?}:{state}:{}", installation.target,
                        installation.status.as_deref().unwrap_or("unknown"))
                } else {
                    format!("{:?}", installation.target)
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let origin = if skill
            .installations
            .iter()
            .all(|installation| installation.managed)
        {
            "Kitter"
        } else {
            "external"
        };
        println!("  {:<28} {:<12} {targets}", skill.name, origin);
    }
    println!("Agents:");
    for estimate in &estimates {
        if let Some(diagnostic) = &estimate.diagnostic {
            println!("  {:<18} unknown: {}", estimate.agent.label(), diagnostic.message);
            continue;
        }
        println!(
            "  {:<18} {:>3} discovered  {:>3} visible  ~{} tokens",
            estimate.agent.label(),
            estimate.discovered_count,
            estimate.model_visible_count,
            estimate.estimated_tokens
        );
    }
    let effective = group_effective_skills(
        estimates
            .iter()
            .flat_map(|estimate| estimate.skills.iter().map(|skill| (estimate.agent, skill))),
    );
    if estimates.iter().any(|estimate| estimate.diagnostic.is_some()) {
        println!("Effective skills: unavailable; any entries below are a partial inventory");
    } else { println!("Effective skills: {}", effective.len()); }
    for group in effective {
        let agents = group
            .entries()
            .iter()
            .map(|(agent, _)| agent.label())
            .collect::<BTreeSet<_>>();
        let plugin = group
            .entries()
            .iter()
            .any(|(_, skill)| skill.source.is_plugin());
        let kind = if plugin { "plugin" } else { "skill" };
        println!(
            "  {:<28} {:<7} {}",
            group.name(),
            kind,
            agents.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    Ok(())
}

fn update_skills(library: &mut SkillLibrary, selectors: &[String], all: bool) -> Result<()> {
    if selectors.is_empty() && !all {
        bail!("请指定至少一个 skill，或使用 --all");
    }
    let skills = if all {
        library.list()?
    } else {
        resolve_skills(library, selectors)?
    };
    let mut updated = 0usize;
    let mut failures = Vec::new();
    for skill in skills {
        match source::update_by_storage(library, &skill.record.storage_name) {
            Ok(()) => updated += 1,
            Err(error) => failures.push(format!("{}：{error:#}", skill.record.name)),
        }
        for warning in library.take_proma_sync_warnings() { eprintln!("{warning}"); }
    }
    finish_batch("Updated", "skill", updated, failures)
}

fn manage_groups(library: &mut SkillLibrary, action: GroupAction) -> Result<()> {
    match action {
        GroupAction::List { json } => {
            let skills = library.list()?;
            let groups = library
                .groups()
                .into_iter()
                .map(|group| GroupOutput {
                    skills: skills
                        .iter()
                        .filter(|skill| skill.record.group_id.as_deref() == Some(&group.id))
                        .count(),
                    id: group.id,
                    name: group.name,
                })
                .collect::<Vec<_>>();
            if json {
                println!("{}", serde_json::to_string_pretty(&groups)?);
            } else if groups.is_empty() {
                println!("No groups");
            } else {
                for group in groups {
                    println!(
                        "{:<28} {}",
                        group.name,
                        counted(group.skills, "skill", "skills")
                    );
                }
            }
            Ok(())
        }
        GroupAction::Create { name } => {
            library.create_group(&name)?;
            Ok(())
        }
        GroupAction::Rename { group, name } => {
            let group = resolve_group(library, &group)?;
            library.rename_group(&group.id, &name)
        }
        GroupAction::Delete {
            group,
            delete_skills,
        } => {
            let group = resolve_group(library, &group)?;
            library.delete_group(&group.id, delete_skills)?;
            Ok(())
        }
        GroupAction::Assign { group, skills } => {
            let group = resolve_group(library, &group)?;
            let skills = resolve_skills(library, &skills)?;
            for skill in skills {
                library.assign_group_by_storage(&skill.record.storage_name, Some(&group.id))?;
            }
            Ok(())
        }
        GroupAction::Clear { skills } => {
            let skills = resolve_skills(library, &skills)?;
            for skill in skills {
                library.assign_group_by_storage(&skill.record.storage_name, None)?;
            }
            Ok(())
        }
    }
}

fn manage_tags(library: &SkillLibrary, action: TagAction) -> Result<()> {
    let (mut skill_tags, project_tags) = load_tag_states();
    let tags = &mut skill_tags;
    match action {
        TagAction::List { json } => {
            let mut output = Vec::new();
            for root in tags.roots() {
                output.push(TagOutput {
                    id: format!("id:{}", root.id),
                    name: root.name.clone(),
                    parent: None,
                    assignments: tags.count(root.id),
                });
                output.extend(tags.children(root.id).map(|child| TagOutput {
                    id: format!("id:{}", child.id),
                    name: child.name.clone(),
                    parent: Some(root.name.clone()),
                    assignments: tags.count(child.id),
                }));
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else if output.is_empty() {
                println!("No tags");
            } else {
                let duplicate_names = output.iter().fold(BTreeMap::new(), |mut counts, tag| {
                    *counts.entry(tag.name.to_lowercase()).or_insert(0usize) += 1;
                    counts
                });
                for tag in output {
                    let indent = if tag.parent.is_some() { "  " } else { "" };
                    let id = if duplicate_names
                        .get(&tag.name.to_lowercase())
                        .copied()
                        .unwrap_or_default()
                        > 1
                    {
                        format!("  {}", tag.id)
                    } else {
                        String::new()
                    };
                    println!(
                        "{indent}#{:<26} {}{id}",
                        tag.name,
                        counted(tag.assignments, "assignment", "assignments")
                    );
                }
            }
            return Ok(());
        }
        TagAction::Create { name, parent } => {
            let parent = parent
                .map(|parent| resolve_tag(tags, &parent))
                .transpose()?;
            tags.add(&name, parent).map_err(anyhow::Error::msg)?;
        }
        TagAction::Rename { tag, name } => {
            let tag = resolve_tag(tags, &tag)?;
            tags.rename(tag, &name).map_err(anyhow::Error::msg)?;
        }
        TagAction::Delete { tag } => {
            let tag = resolve_tag(tags, &tag)?;
            tags.delete(tag);
        }
        TagAction::Assign { tag, items } => {
            let tag = resolve_tag(tags, &tag)?;
            for key in tag_keys(library, &items)? {
                tags.set_assignment(&key, tag, true);
            }
        }
        TagAction::Unassign { tag, items } => {
            let tag = resolve_tag(tags, &tag)?;
            for key in tag_keys(library, &items)? {
                tags.set_assignment(&key, tag, false);
            }
        }
        TagAction::Move { tag, before, after } => {
            let tag = resolve_tag(tags, &tag)?;
            let moved = if let Some(target) = before {
                let target = resolve_tag(tags, &target)?;
                tags.move_before(tag, target)
            } else if let Some(target) = after {
                let target = resolve_tag(tags, &target)?;
                tags.move_after(tag, target)
            } else {
                false
            };
            if !moved {
                bail!("只能在同一级标签之间调整顺序");
            }
        }
    }
    save_tag_states(&skill_tags, &project_tags)
}

fn manage_library(
    library: &mut SkillLibrary,
    set: Option<PathBuf>,
    move_to: Option<PathBuf>,
) -> Result<()> {
    if let Some(path) = move_to {
        if !path.is_absolute() {
            bail!("请使用绝对路径");
        }
        let plan = library.plan_storage_change(kitter::library::StorageAction::MoveLibrary, &path)?;
        library.apply_storage_change(&plan)?;
    }
    if let Some(path) = set {
        if !path.is_absolute() {
            bail!("请使用绝对路径");
        }
        if path != library.config.library_dir {
            let plan = library.plan_storage_change(kitter::library::StorageAction::UseLibrary, &path)?;
            library.apply_storage_change(&plan)?;
        }
    }
    println!("{}", library.config.library_dir.display());
    Ok(())
}

fn open_library_snapshot(data_dir: &Path) -> Result<(SkillLibrary, tempfile::TempDir)> {
    let config = kitter::config::AppConfig::load_from(data_dir)?;
    let snapshot = tempfile::tempdir()?;
    let registry = data_dir.join("registry.json");
    if registry.exists() {
        std::fs::copy(registry, snapshot.path().join("registry.json"))?;
    }
    let mut library = SkillLibrary::open_in(snapshot.path())?;
    library.config = config;
    Ok((library, snapshot))
}

fn proma_workspace(root: &Path, slug: &str) -> Result<proma::Workspace> {
    proma::workspaces(root)?.into_iter()
        .find(|workspace| workspace.slug == slug)
        .with_context(|| format!("没有找到已登记的 Proma 工作区：{slug}"))
}

fn manage_proma(action: PromaAction) -> Result<()> {
    let root = proma::home().context("无法定位 Proma 配置根")?;
    // Normal library opening can upgrade the builtin and registry. Resolve
    // selectors in a temporary snapshot; inspections/recovery need no library.
    let snapshot = if matches!(&action, PromaAction::Plan { .. } | PromaAction::Sync { .. }
        | PromaAction::Enable { .. } | PromaAction::Disable { .. }) {
        Some(open_library_snapshot(&kitter::config::app_data_dir())?)
    } else {
        None
    };
    let library = snapshot.as_ref().map(|(library, _)| library);
    match action {
        PromaAction::Workspaces { json } => {
            let workspaces = proma::workspaces(&root)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&workspaces)?);
            } else {
                for workspace in workspaces {
                    println!("{}  {}  {}", workspace.slug, workspace.name, workspace.path.display());
                }
            }
            Ok(())
        }
        PromaAction::Status { workspace, json } => {
            let workspace = proma_workspace(&root, &workspace)?;
            let installations = proma::inspect(&workspace.path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "workspace": workspace,
                    "evidence": "static_inspection",
                    "runtime_verified": false,
                    "installations": installations,
                }))?);
            } else {
                println!("Proma: {}（静态检查，未验证运行时加载）", workspace.slug);
                for installation in installations {
                    println!("{:<28} {:<10} {:<16} {}", installation.name,
                        installation.state, installation.status, installation.path.display());
                }
            }
            Ok(())
        }
        PromaAction::Plan { workspace, skills } => {
            let workspace = proma_workspace(&root, &workspace)?;
            let plans = resolve_skills(library.context("命令需要技能库")?, &skills)?.iter().map(|skill| {
                proma::plan_install(&root, &workspace.path, &skill.path, &skill.record.name)
            }).collect::<Result<Vec<_>>>()?;
            println!("{}", serde_json::to_string_pretty(&plans)?);
            Ok(())
        }
        PromaAction::Sync { workspace, skills } => {
            let workspace = proma_workspace(&root, &workspace)?;
            let skills = resolve_skills(library.context("命令需要技能库")?, &skills)?;
            for skill in &skills {
                proma::plan_install(&root, &workspace.path, &skill.path, &skill.record.name)?;
            }
            let mut succeeded = 0;
            let mut failures = Vec::new();
            for skill in skills {
                match proma::install(&workspace.path, &skill.path, &skill.record.name) {
                    Ok(()) => succeeded += 1,
                    Err(error) => failures.push(format!("{}：{error:#}", skill.record.name)),
                }
            }
            finish_batch("Synced", "skill", succeeded, failures)
        }
        PromaAction::Enable { workspace, skills } => {
            set_proma_enabled(library.context("命令需要技能库")?, &root, &workspace, &skills, true)
        }
        PromaAction::Disable { workspace, skills } => {
            set_proma_enabled(library.context("命令需要技能库")?, &root, &workspace, &skills, false)
        }
        PromaAction::Recover { workspace } => {
            let workspace = proma_workspace(&root, &workspace)?;
            proma::recover(&workspace.path)?;
            println!("Recovered {}", workspace.slug);
            Ok(())
        }
    }
}

fn set_proma_enabled(
    library: &SkillLibrary, root: &Path, slug: &str, selectors: &[String], enabled: bool,
) -> Result<()> {
    let workspace = proma_workspace(root, slug)?;
    let skills = resolve_skills(library, selectors)?;
    let mut succeeded = 0;
    let mut failures = Vec::new();
    for skill in skills {
        let directory = if enabled { "skills-inactive" } else { "skills" };
        let path = workspace.path.join(directory).join(&skill.record.name);
        if !proma::is_managed(&path, &skill.path) {
            failures.push(format!("{}：没有与库来源匹配的受管安装", skill.record.name));
            continue;
        }
        match proma::set_enabled(&workspace.path, &skill.record.name, enabled) {
            Ok(()) => succeeded += 1,
            Err(error) => failures.push(format!("{}：{error:#}", skill.record.name)),
        }
    }
    finish_batch(if enabled { "Enabled" } else { "Disabled" }, "skill", succeeded, failures)
}

fn resolve_skills(library: &SkillLibrary, selectors: &[String]) -> Result<Vec<SkillSummary>> {
    let mut seen = HashSet::new();
    let mut skills = Vec::new();
    for selector in selectors {
        let skill = library.resolve_skill(selector)?;
        if seen.insert(skill.record.storage_name.clone()) {
            skills.push(skill);
        }
    }
    Ok(skills)
}

fn matching_library_skills<'a>(
    skills: &'a [SkillSummary],
    selector: &str,
) -> Vec<&'a SkillSummary> {
    if let Some(storage_name) = selector.strip_prefix("id:") {
        return skills
            .iter()
            .filter(|skill| skill.record.storage_name == storage_name)
            .collect();
    }
    skills
        .iter()
        .filter(|skill| skill.record.name == selector)
        .collect()
}

fn resolve_group(library: &SkillLibrary, selector: &str) -> Result<SkillGroup> {
    library
        .groups()
        .into_iter()
        .find(|group| group.id == selector || group.name.eq_ignore_ascii_case(selector))
        .with_context(|| format!("没有找到分组：{selector}"))
}

fn resolve_tag(tags: &TagState, selector: &str) -> Result<TagId> {
    let selector = selector.trim().trim_start_matches('#').trim();
    if let Some(id) = selector.strip_prefix("id:") {
        let id = id
            .parse::<TagId>()
            .with_context(|| format!("无效的标签 ID：{selector}"))?;
        return tags
            .tag(id)
            .map(|tag| tag.id)
            .with_context(|| format!("没有找到标签：{selector}"));
    }
    let matches = tags
        .tags()
        .iter()
        .filter(|tag| tag.name.eq_ignore_ascii_case(selector))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [tag] => Ok(tag.id),
        [] => bail!("没有找到标签：{selector}"),
        _ => bail!("存在多个名为 {selector} 的标签，请使用 list 中显示的 ID"),
    }
}

fn tag_keys(library: &SkillLibrary, items: &[String]) -> Result<Vec<String>> {
    Ok(resolve_skills(library, items)?
        .into_iter()
        .map(|skill| skill.record.storage_name)
        .collect())
}

fn canonical_project(path: &Path) -> Result<PathBuf> {
    if !path.is_dir() {
        bail!("项目文件夹不存在：{}", path.display());
    }
    path.canonicalize()
        .with_context(|| format!("无法读取项目：{}", path.display()))
}

fn same_file(left: &Path, right: &Path) -> bool {
    left.canonicalize().ok() == right.canonicalize().ok()
}

fn finish_batch(action: &str, item: &str, succeeded: usize, failures: Vec<String>) -> Result<()> {
    if failures.is_empty() {
        println!("{action} {}", counted(succeeded, item, &format!("{item}s")));
        return Ok(());
    }
    if succeeded > 0 {
        eprintln!("{action} {}", counted(succeeded, item, &format!("{item}s")));
    }
    bail!(
        "{} failed: {}",
        counted(failures.len(), "operation", "operations"),
        failures.join("; ")
    )
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};
    use kitter::tags::TagState;

    use super::{Cli, Command, ProjectViewArg, PromaAction, TagAction, open_library_snapshot, resolve_tag};

    #[test]
    fn command_tree_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_batch_install() {
        let cli = Cli::try_parse_from([
            "kitter",
            "install",
            "alpha",
            "beta",
            "--project",
            "/tmp/project",
            "--target",
            "universal",
            "--target",
            "codex",
        ])
        .unwrap();
        let Command::Install { skills, target, .. } = cli.command else {
            panic!("expected install command");
        };
        assert_eq!(skills, ["alpha", "beta"]);
        assert_eq!(target.len(), 2);
    }

    #[test]
    fn parses_project_plugin_view() {
        let cli = Cli::try_parse_from([
            "kitter",
            "project",
            "/tmp/project",
            "--agent",
            "codex",
            "--view",
            "plugins",
        ])
        .unwrap();
        let Command::Project { view, .. } = cli.command else {
            panic!("expected project command");
        };
        assert!(matches!(view, ProjectViewArg::Plugins));
    }

    #[test]
    fn proma_commands_require_one_explicit_workspace() {
        for action in ["plan", "sync", "enable", "disable"] {
            let cli = Cli::try_parse_from(["kitter", "proma", action,
                "--workspace", "ai-cube-pilot", "alpha", "beta"]).unwrap();
            assert!(matches!(cli.command, Command::Proma { .. }));
            assert!(Cli::try_parse_from(["kitter", "proma", action, "alpha"]).is_err());
            assert!(Cli::try_parse_from(["kitter", "proma", action,
                "--workspace", "ai-cube-pilot"]).is_err());
        }
        let cli = Cli::try_parse_from(["kitter", "proma", "recover", "--workspace", "ai-cube-pilot"]).unwrap();
        assert!(matches!(cli.command, Command::Proma { action: PromaAction::Recover { .. } }));
        assert!(Cli::try_parse_from(["kitter", "proma", "recover"]).is_err());
        assert!(Cli::try_parse_from(["kitter", "install", "alpha", "--project", "/tmp/native", "--target", "proma"]).is_ok());
        assert!(Cli::try_parse_from(["kitter", "project", "/tmp/native", "--agent", "proma"]).is_ok());
    }

    #[test]
    fn proma_library_snapshot_does_not_initialize_or_upgrade_live_library() {
        let temp = tempfile::tempdir().unwrap();
        let (snapshot_library, _snapshot) = open_library_snapshot(temp.path()).unwrap();
        assert_eq!(snapshot_library.config.library_dir, temp.path().join("skills"));
        assert!(std::fs::read_dir(temp.path()).unwrap().next().is_none());

        let mut library = kitter::SkillLibrary::open_in(temp.path()).unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("SKILL.md"), "---\nname: alpha\ndescription: fixture\n---\n").unwrap();
        kitter::source::scan_local(&source).unwrap()
            .import_selected(&mut library, &std::collections::HashSet::from(["alpha".into()]), None).unwrap();
        let registry_before = std::fs::read(temp.path().join("registry.json")).unwrap();
        let config_before = std::fs::read(temp.path().join("config.json")).unwrap();
        let builtin = library.config.library_dir.join("_kitter-builtin/SKILL.md");
        std::fs::write(&builtin, "old fixture builtin").unwrap();
        let (snapshot_library, _snapshot) = open_library_snapshot(temp.path()).unwrap();
        assert_eq!(snapshot_library.resolve_skill("alpha").unwrap().path, library.resolve_skill("alpha").unwrap().path);
        assert_eq!(std::fs::read(temp.path().join("registry.json")).unwrap(), registry_before);
        assert_eq!(std::fs::read(temp.path().join("config.json")).unwrap(), config_before);
        assert_eq!(std::fs::read_to_string(builtin).unwrap(), "old fixture builtin");
    }

    #[test]
    fn parses_skill_tag_assignment() {
        let cli = Cli::try_parse_from(["kitter", "tag", "assign", "client", "skill-a", "skill-b"])
            .unwrap();
        let Command::Tag {
            action: TagAction::Assign { items, .. },
        } = cli.command
        else {
            panic!("expected skill tag assignment");
        };
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn client_settings_are_not_cli_commands() {
        assert!(Cli::try_parse_from(["kitter", "config", "set-theme", "dark"]).is_err());
        let cli = Cli::try_parse_from(["kitter", "library", "--set", "/tmp/skills"])
            .expect("skill library location should remain configurable");
        let Command::Library { set, .. } = cli.command else {
            panic!("expected library command");
        };
        assert_eq!(set.unwrap(), std::path::PathBuf::from("/tmp/skills"));
    }

    #[test]
    fn library_set_does_not_reuse_old_origins_for_existing_same_named_skills() {
        use super::{SkillLibrary, manage_library};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let data = root.join("data");
        let library = SkillLibrary::open_in(&data).unwrap();
        let old_skill = library.config.library_dir.join("demo");
        std::fs::create_dir(&old_skill).unwrap();
        std::fs::write(old_skill.join("SKILL.md"), "old fixture skill").unwrap();
        let mut registry: serde_json::Value = serde_json::from_slice(&std::fs::read(data.join("registry.json")).unwrap()).unwrap();
        registry["skills"]["demo"] = serde_json::to_value(kitter::SkillRecord {
            name: "demo".into(), storage_name: "demo".into(), description: String::new(),
            origin: kitter::SkillOrigin::Npx { repository: "fixture/remote".into(), skill: "demo".into(), source_hash: None },
            update_available: true, group_id: None, last_operated_at: 0,
        }).unwrap();
        std::fs::write(data.join("registry.json"), serde_json::to_vec(&registry).unwrap()).unwrap();
        let mut library = SkillLibrary::open_in(&data).unwrap();
        assert!(matches!(library.record_by_storage("demo").unwrap().origin, kitter::SkillOrigin::Npx { .. }));
        let target = root.join("existing");
        std::fs::create_dir_all(target.join("demo")).unwrap();
        std::fs::write(target.join("demo/SKILL.md"), "different target fixture").unwrap();
        manage_library(&mut library, Some(target.clone()), None).unwrap();
        assert!(matches!(library.record_by_storage("demo").unwrap().origin, kitter::SkillOrigin::Unknown));
        assert_eq!(std::fs::read(old_skill.join("SKILL.md")).unwrap(), b"old fixture skill");
        let reopened = SkillLibrary::open_in(&data).unwrap();
        assert!(matches!(reopened.record_by_storage("demo").unwrap().origin, kitter::SkillOrigin::Unknown));
        assert!(manage_library(&mut library, Some(root.join("empty")), None).is_err());
        assert!(!root.join("empty").exists());
        assert_eq!(library.config.library_dir, target);
    }

    #[test]
    fn library_move_is_parsed_and_conflicts_with_set() {
        let cli = Cli::try_parse_from(["kitter", "library", "--move", "/tmp/moved-skills"])
            .expect("library move should be a public CLI operation");
        let Command::Library { set, move_to } = cli.command else {
            panic!("expected library command");
        };
        assert!(set.is_none());
        assert_eq!(move_to.unwrap(), std::path::PathBuf::from("/tmp/moved-skills"));
        assert!(Cli::try_parse_from([
            "kitter", "library", "--set", "/tmp/existing", "--move", "/tmp/moved-skills",
        ]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn library_move_cli_preserves_payload_local_data_and_installed_link() {
        use super::{SkillLibrary, manage_library};
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let data = root.join("data");
        let mut library = SkillLibrary::open_in(&data).unwrap();
        let template = root.join("template");
        std::fs::create_dir(&template).unwrap();
        let body = b"---\nname: demo\ndescription: Owned move fixture\n---\nOwned body\n";
        std::fs::write(template.join("SKILL.md"), body).unwrap();
        library.import(&template, kitter::SkillRecord {
            name: "demo".into(), storage_name: "demo".into(), description: String::new(),
            origin: kitter::SkillOrigin::Unknown, update_available: false,
            group_id: None, last_operated_at: 0,
        }).unwrap();
        // Reconcile the imported source before taking the migration baseline,
        // just as opening the library for a separate CLI invocation does.
        library = SkillLibrary::open_in(&data).unwrap();
        let old = library.config.library_dir.clone();
        let skill = library.skill_path("demo").unwrap();
        std::fs::create_dir_all(skill.join("local-data")).unwrap();
        std::fs::write(skill.join("local-data/ADAPTATION.md"), b"Owned adaptation\n").unwrap();
        std::fs::write(skill.join("local-data/sentinel.bin"), b"private\x00\xff").unwrap();
        std::fs::set_permissions(skill.join("local-data/sentinel.bin"),
            std::fs::Permissions::from_mode(0o751)).unwrap();
        let identity = |path: &std::path::Path| {
            let info = std::fs::symlink_metadata(path).unwrap();
            (info.dev(), info.ino(), info.uid(), info.mode())
        };
        let old_identity = identity(&old);
        let files = ["SKILL.md", "local-data/ADAPTATION.md", "local-data/sentinel.bin"];
        let before = files.map(|relative| {
            let path = skill.join(relative);
            (std::fs::read(&path).unwrap(), identity(&path))
        });
        let installation = root.join("installed-demo");
        symlink(&skill, &installation).unwrap();
        let installation_identity = identity(&installation);
        let installation_target = std::fs::read_link(&installation).unwrap();
        let registry: serde_json::Value = serde_json::from_slice(
            &std::fs::read(data.join("registry.json")).unwrap()).unwrap();
        let target = root.join("library-moved");
        let cli = Cli::try_parse_from([
            "kitter", "library", "--move", target.to_str().unwrap(),
        ]).unwrap();
        let Command::Library { set, move_to } = cli.command else {
            panic!("expected library command");
        };
        manage_library(&mut library, set, move_to).unwrap();
        assert_eq!(library.config.library_dir, target);
        assert_eq!(identity(&target), old_identity);
        assert!(std::fs::symlink_metadata(&old).unwrap().file_type().is_symlink());
        assert_eq!(old.canonicalize().unwrap(), target);
        assert_eq!(identity(&installation), installation_identity);
        assert_eq!(std::fs::read_link(&installation).unwrap(), installation_target);
        let after_move: serde_json::Value = serde_json::from_slice(
            &std::fs::read(data.join("registry.json")).unwrap()).unwrap();
        assert_eq!(after_move, registry);
        let reopened = SkillLibrary::open_in(&data).unwrap();
        assert_eq!(reopened.config.library_dir, target);
        let moved_skill = reopened.skill_path("demo").unwrap();
        assert!(moved_skill.starts_with(&target));
        for (relative, (bytes, file_identity)) in files.iter().zip(before.iter()) {
            let moved = moved_skill.join(relative);
            assert_eq!(std::fs::read(&moved).unwrap(), *bytes);
            assert_eq!(identity(&moved), *file_identity);
            assert_eq!(std::fs::read(installation.join(relative)).unwrap(), *bytes);
            assert_eq!(std::fs::read(skill.join(relative)).unwrap(), *bytes);
        }
        let after: serde_json::Value = serde_json::from_slice(
            &std::fs::read(data.join("registry.json")).unwrap()).unwrap();
        assert_eq!(after, registry);
        assert_eq!(std::fs::read(template.join("SKILL.md")).unwrap(), body);
    }

    #[test]
    fn tags_use_names_and_only_require_ids_for_duplicates() {
        let mut tags = TagState::default();
        let work = tags.add("work", None).unwrap();
        let personal = tags.add("personal", None).unwrap();
        let work_client = tags.add("client", Some(work)).unwrap();
        tags.add("client", Some(personal)).unwrap();

        assert_eq!(resolve_tag(&tags, "work").unwrap(), work);
        assert!(resolve_tag(&tags, "client").is_err());
        assert_eq!(
            resolve_tag(&tags, &format!("id:{work_client}")).unwrap(),
            work_client
        );
    }
}
