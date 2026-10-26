//! Hermes filesystem catalog. Runtime quarantine/app/session gates are unknown.
use super::*;
use crate::agents::HermesSnapshot;
use anyhow::{Context, Result, ensure};

const EXCLUDED: &[&str] = &[
    ".git",
    ".github",
    ".hub",
    ".archive",
    ".curator_backups",
    ".locks",
    ".venv",
    "venv",
    "node_modules",
    "site-packages",
    "__pycache__",
    ".tox",
    ".nox",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
];
const SUPPORT: &[&str] = &["references", "templates", "assets", "scripts"];

struct Candidate {
    skill: EffectiveSkill,
    tier: u8,
    relative: String,
    bytes: Vec<u8>,
    supported: bool,
}

fn files(root: &Path) -> Result<Vec<PathBuf>> {
    if root
        .components()
        .any(|part| EXCLUDED.contains(&part.as_os_str().to_string_lossy().as_ref()))
    {
        return Ok(Vec::new());
    }
    match fs::metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => anyhow::bail!("Hermes 技能根不可读，状态未知"),
        Ok(metadata) => ensure!(metadata.is_dir(), "Hermes 技能根不是目录，状态未知"),
    }
    let mut result = Vec::new();
    let walker = walkdir::WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !EXCLUDED.contains(&name.as_ref())
                && !(entry.file_type().is_dir()
                    && SUPPORT.contains(&name.as_ref())
                    && entry
                        .path()
                        .parent()
                        .is_some_and(|parent| parent.join("SKILL.md").is_file()))
        });
    for (count, entry) in walker.enumerate() {
        ensure!(count < 100_000, "Hermes 技能扫描超出上限，状态未知");
        let entry = entry.map_err(|_| anyhow::anyhow!("Hermes 技能目录遍历失败，状态未知"))?;
        if entry.file_type().is_file() && entry.file_name() == "SKILL.md" {
            result.push(entry.into_path());
        }
    }
    result.sort();
    Ok(result)
}

fn list(value: Option<&serde_yaml::Value>) -> Vec<String> {
    match value {
        Some(serde_yaml::Value::String(value)) => vec![value.clone()],
        Some(serde_yaml::Value::Sequence(values)) => values
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

/// Hermes uses ast.literal_eval for quoted lists, then trims every name.
/// JSON5 handles the common Python string-list syntax (single quotes and a
/// trailing comma). Restrict it to that shared grammar; unsupported literals
/// become unknown instead of confidently enabling a possibly disabled skill.
fn disabled_names(value: Option<&serde_yaml::Value>) -> Result<HashSet<String>> {
    fn name(value: &serde_yaml::Value) -> Result<String> {
        match value {
            serde_yaml::Value::String(s) => Ok(s.clone()),
            serde_yaml::Value::Bool(v) => Ok(if *v { "True" } else { "False" }.into()),
            // YAML display (.nan, .inf, exponent spelling) is not Python str.
            // A differing name must never make an actually disabled skill Automatic.
            serde_yaml::Value::Number(_) => anyhow::bail!("Hermes 数值禁用名称无法静态等价规范化，状态未知"),
            serde_yaml::Value::Null => Ok("None".into()),
            _ => anyhow::bail!("Hermes 禁用名称结构无法静态规范化，状态未知"),
        }
    }
    let values = match value {
        Some(serde_yaml::Value::String(raw)) if raw.trim_start().starts_with('[') => {
            let mut chars = raw.trim().chars().peekable();
            ensure!(chars.next() == Some('['), "Hermes 禁用名单格式未知");
            loop {
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
                if chars.peek() == Some(&']') {
                    chars.next();
                    break;
                }
                let quote = chars.next().context("Hermes 禁用名单未结束")?;
                ensure!(
                    matches!(quote, '\'' | '"'),
                    "Hermes 禁用名单字面量无法静态规范化，状态未知"
                );
                let mut closed = false;
                while let Some(c) = chars.next() {
                    if c == quote {
                        closed = true;
                        break;
                    }
                    if c == '\\' {
                        let escape = chars.next().context("Hermes 禁用名称转义未结束")?;
                        ensure!(
                            matches!(
                                escape,
                                '\\' | '\'' | '"' | 'b' | 'f' | 'n' | 'r' | 't' | 'v' | 'u' | 'x'
                            ),
                            "Hermes 禁用名称转义无法静态规范化，状态未知"
                        );
                        if escape == 'u' {
                            let digits = chars.by_ref().take(4).collect::<String>();
                            let code = u32::from_str_radix(&digits, 16).ok();
                            ensure!(
                                digits.len() == 4
                                    && code.is_some_and(|v| !(0xd800..=0xdfff).contains(&v)),
                                "Hermes 禁用名称 Unicode 转义无法静态规范化，状态未知"
                            );
                        }
                    }
                }
                ensure!(closed, "Hermes 禁用名称未结束");
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
                match chars.next() {
                    Some(',') => {}
                    Some(']') => break,
                    _ => anyhow::bail!("Hermes 禁用名单字面量无法静态规范化，状态未知"),
                }
            }
            ensure!(
                chars.all(char::is_whitespace),
                "Hermes 禁用名单尾部结构未知"
            );
            json5::from_str::<Vec<String>>(raw)
                .map_err(|_| anyhow::anyhow!("Hermes 禁用名单无法静态规范化，状态未知"))?
        }
        Some(serde_yaml::Value::String(raw)) => vec![raw.clone()],
        Some(serde_yaml::Value::Sequence(values)) => {
            values.iter().map(name).collect::<Result<Vec<_>>>()?
        }
        _ => Vec::new(),
    };
    Ok(values
        .into_iter()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .collect())
}

fn winner(candidates: &[Candidate], indexes: &[usize]) -> Option<usize> {
    let tier = indexes.iter().map(|i| candidates[*i].tier).min()?;
    let mut top = indexes
        .iter()
        .copied()
        .filter(|i| candidates[*i].tier == tier)
        .collect::<Vec<_>>();
    if top.len() == 1 {
        return top.first().copied();
    }
    let first = &candidates[top[0]];
    if !top.iter().all(|i| {
        candidates[*i].skill.root_path == first.skill.root_path
            && candidates[*i].bytes == first.bytes
    }) {
        return None;
    }
    top.sort_by_key(|i| candidates[*i].relative.split('/').count());
    (candidates[top[0]].relative.split('/').count()
        != candidates[top[1]].relative.split('/').count())
    .then_some(top[0])
}

fn collect(snapshot: &HermesSnapshot) -> Result<(Vec<EffectiveSkill>, bool)> {
    let mut candidates = Vec::<Candidate>::new();
    let mut runtime_unknown = false;
    for (root, tier) in &snapshot.roots {
        for file in files(root)? {
            ensure!(
                file.metadata().is_ok_and(|m| m.len() <= 2 * 1024 * 1024),
                "Hermes SKILL.md 超出静态读取范围，状态未知"
            );
            let bytes =
                fs::read(&file).map_err(|_| anyhow::anyhow!("Hermes SKILL.md 不可读，状态未知"))?;
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| anyhow::anyhow!("Hermes SKILL.md 编码无效，状态未知"))?
                .trim_start_matches('\u{feff}');
            let yaml = text
                .strip_prefix("---")
                .and_then(|s| s.split_once("\n---"))
                .map(|(header, _)| header);
            let fm = match yaml {
                Some(header) => serde_yaml::from_str::<serde_yaml::Value>(header)
                    .map_err(|_| anyhow::anyhow!("Hermes 技能元数据无法静态解析，状态未知"))?,
                None => serde_yaml::Value::Null,
            };
            let dir = file.parent().context("Hermes 技能路径无效")?;
            let name = fm
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| dir.file_name().and_then(|s| s.to_str()).unwrap_or(""));
            if name.is_empty() {
                continue;
            }
            let body = text
                .strip_prefix("---")
                .and_then(|s| s.split_once("\n---"))
                .map(|(_, body)| body)
                .unwrap_or(text);
            let description = fm
                .get("description")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    body.lines()
                        .map(str::trim)
                        .find(|s| !s.is_empty() && !s.starts_with('#'))
                        .unwrap_or("")
                })
                .chars()
                .take(1024)
                .collect::<String>();
            let name = name.chars().take(64).collect::<String>();
            let platforms = list(fm.get("platforms"));
            let os = if cfg!(target_os = "macos") {
                "darwin"
            } else if cfg!(windows) {
                "win32"
            } else {
                std::env::consts::OS
            };
            let supported = platforms.is_empty()
                || platforms.iter().any(|p| {
                    let p = p.trim().to_lowercase();
                    os.starts_with(match p.as_str() {
                        "macos" => "darwin",
                        "windows" => "win32",
                        _ => &p,
                    })
                });
            // Resolution must see unsupported/disabled owners too: their
            // presence must not revive a shadowed lower-tier skill.
            let needs_runtime = *tier == 0
                || fm.get("environments").is_some_and(|v| !v.is_null())
                || fm.get("requires_apps").is_some_and(|v| !v.is_null());
            runtime_unknown |= needs_runtime;
            let relative = dir
                .strip_prefix(root)
                .context("Hermes 技能不在扫描根内")?
                .to_string_lossy()
                .replace('\\', "/");
            candidates.push(Candidate {
                tier: *tier,
                relative,
                skill: EffectiveSkill {
                    id: name.clone(),
                    name: name.clone(),
                    description,
                    when_to_use: None,
                    path: file,
                    root_path: Some(root.clone()),
                    prompt_path: None,
                    scope: if *tier == 0 {
                        SkillScope::Repository
                    } else {
                        SkillScope::User
                    },
                    visibility: if !supported {
                        SkillVisibility::ManualOnly
                    } else if needs_runtime {
                        SkillVisibility::Conditional
                    } else {
                        SkillVisibility::Automatic
                    },
                    user_invocable: true,
                    source: SkillSource::Filesystem,
                },
                bytes,
                supported,
            });
        }
    }
    let mut aliases = BTreeMap::<String, Vec<usize>>::new();
    for (i, candidate) in candidates.iter().enumerate() {
        let basename = candidate
            .skill
            .path
            .parent()
            .and_then(|p| p.file_name())
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        for alias in HashSet::from([
            candidate.skill.name.clone(),
            candidate.relative.clone(),
            basename,
        ]) {
            aliases.entry(alias).or_default().push(i);
        }
    }
    let winners = aliases
        .iter()
        .map(|(name, indexes)| (name.clone(), winner(&candidates, indexes)))
        .collect::<BTreeMap<_, _>>();
    let mut disabled = disabled_names(snapshot.skills.get("disabled"))?;
    if let Some(platform) = env::var("HERMES_PLATFORM")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            env::var("HERMES_SESSION_PLATFORM")
                .ok()
                .filter(|s| !s.is_empty())
        })
    {
        disabled.extend(disabled_names(
            snapshot
                .skills
                .get("platform_disabled")
                .and_then(|v| v.get(platform.as_str())),
        )?);
    }
    disabled.retain(|name| name != "hermes-agent");
    let tiers = candidates
        .iter()
        .map(|candidate| candidate.tier)
        .collect::<Vec<_>>();
    let mut result = Vec::new();
    for (i, mut candidate) in candidates.into_iter().enumerate() {
        let own = winners.get(&candidate.skill.name).copied().flatten();
        let higher = aliases[&candidate.skill.name]
            .iter()
            .any(|j| tiers[*j] < candidate.tier);
        let load_name = if own == Some(i) {
            Some(candidate.skill.name.clone())
        } else if own.is_none() && winners.get(&candidate.relative).copied().flatten() == Some(i) {
            Some(candidate.relative.clone())
        } else {
            None
        };
        if higher || own.is_some_and(|owner| owner != i) {
            candidate.skill.visibility = SkillVisibility::Shadowed;
        } else if load_name.is_none() {
            candidate.skill.visibility = SkillVisibility::Conditional;
            runtime_unknown = true;
        }
        if disabled.contains(&candidate.skill.name)
            || load_name
                .as_ref()
                .is_some_and(|name| disabled.contains(name))
        {
            continue;
        }
        if !candidate.supported {
            continue;
        }
        if let Some(name) = load_name {
            candidate.skill.id = name;
        }
        result.push(candidate.skill);
    }
    Ok((result, runtime_unknown))
}

pub(super) fn estimate(context: &DiscoveryContext) -> AgentContextEstimate {
    let result = crate::agents::hermes_snapshot(&context.home, &context.cwd)
        .and_then(|snapshot| collect(&snapshot));
    match result {
        Ok((skills, unknown)) => {
            let mut estimate = estimate_skills(&HermesPolicy, skills);
            if unknown {
                estimate.diagnostic = Some(SkillDiscoveryDiagnostic { code: "hermes_native_eligibility_unknown".into(),
                message: "已列出静态候选；Hermes 项目隔离检查或会话应用/环境条件需在原生宿主核验，实际数量未知".into(), metadata_only: Vec::new() });
            }
            estimate
        }
        Err(_) => {
            let mut estimate = estimate_skills(&HermesPolicy, Vec::new());
            estimate.diagnostic = Some(SkillDiscoveryDiagnostic { code: "hermes_source_unknown".into(),
                message: "Hermes profile、配置或技能来源不可读/不受支持；有效技能状态未知，请核对当前 HERMES_HOME 后重试".into(), metadata_only: Vec::new() });
            estimate
        }
    }
}
