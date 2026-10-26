//! WorkBuddy 5.8.2 (official installed package) and TRAE CN entry rules.
//! Evidence and file hashes: docs/agent-compatibility/native-guides/desktop.json.
//! Scan profiles belong to adapters; these policies never walk skill trees.
use super::*;

pub(super) struct WorkBuddyPolicy;
/// Only the Chinese distribution's global root is verified.
pub(super) struct TraePolicy;

/// WorkBuddy 5.8.2: parseSkillFileImpl / coerceFrontmatterText. This is
/// metadata parsing only; directory traversal remains in the shared scanner.
pub(super) fn workbuddy_metadata(path: &Path) -> Option<SkillMetadata> {
    let content = fs::read_to_string(path).ok()?;
    let dir = path.parent()?;
    let basename = dir.file_name()?.to_string_lossy().into_owned();
    let installed = workbuddy_installed_metadata(dir);
    let installed_name = installed
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let (parsed, body) = workbuddy_document(&content);
    let Ok(fm) = parsed else {
        // Native catch returns an inventory entry even for malformed YAML.
        return Some(SkillMetadata {
            name: installed_name.unwrap_or(&basename).to_owned(),
            description: basename,
            has_explicit_description: false,
            when_to_use: None,
            source_path: path.to_path_buf(),
        });
    };
    let name = workbuddy_text(&fm["name"]).filter(|s| !s.is_empty());
    let description = workbuddy_text(&fm["description"]).filter(|s| !s.is_empty());
    let has_explicit_description = description.is_some();
    // JS slice counts UTF-16 code units, including a possible lone surrogate.
    // Rust strings represent that last case with U+FFFD via from_utf16_lossy.
    let description = description.unwrap_or_else(|| {
        let units = body
            .trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
            .encode_utf16()
            .take(100)
            .collect::<Vec<_>>();
        if units.is_empty() {
            basename.clone()
        } else {
            String::from_utf16_lossy(&units)
        }
    });
    Some(SkillMetadata {
        name: installed_name
            .map(str::to_owned)
            .or(name)
            .unwrap_or(basename),
        description,
        has_explicit_description,
        when_to_use: workbuddy_text(&fm["when-to-use"]),
        source_path: path.to_path_buf(),
    })
}

fn workbuddy_installed_metadata(dir: &Path) -> serde_json::Value {
    for file in [
        "_skillhub_meta.json",
        "_marketplace_meta.json",
        "_knot_meta.json",
        "_builtin_market_meta.json",
        "_user_meta.json",
    ] {
        let path = dir.join(file);
        // Native selects the first filename present, then returns null on any
        // read/parse failure. A corrupt high-priority file must not fall through.
        if fs::symlink_metadata(&path).is_ok() {
            return fs::read_to_string(path)
                .ok()
                .and_then(|content| serde_json::from_str(&content).ok())
                .unwrap_or_default();
        }
    }
    serde_json::Value::Null
}

fn workbuddy_document(content: &str) -> (Result<serde_yaml::Value, ()>, &str) {
    // The native /^---\r?\n([\s\S]*?)\r?\n---/ accepts an EOF closing
    // delimiter and leaves any suffix after the closing three dashes as body.
    let Some(rest) = content
        .strip_prefix("---\r\n")
        .or_else(|| content.strip_prefix("---\n"))
    else {
        return (Ok(serde_yaml::Value::Null), content);
    };
    let Some(end) = rest.find("\n---") else {
        return (Ok(serde_yaml::Value::Null), content);
    };
    let fm = rest[..end].strip_suffix('\r').unwrap_or(&rest[..end]);
    let body = &rest[end + 4..];
    let body = body
        .strip_prefix("\r\n")
        .or_else(|| body.strip_prefix('\n'))
        .unwrap_or(body);
    let parsed = workbuddy_flat_frontmatter(fm)
        .map(Ok)
        .unwrap_or_else(|| serde_yaml::from_str(fm).map_err(|_| ()));
    (parsed, body)
}

fn workbuddy_flat_frontmatter(source: &str) -> Option<serde_yaml::Value> {
    let mut result = serde_yaml::Mapping::new();
    for line in source.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with(char::is_whitespace) || line.starts_with('-') {
            return None;
        }
        let (key, raw) = line.split_once(':')?;
        if !key
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
            || !key
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
            || (!raw.is_empty() && !raw.starts_with(' '))
        {
            return None;
        }
        let raw = raw.trim();
        let raw = if raw.starts_with(['\"', '\'']) {
            raw
        } else {
            raw.split_once(" #")
                .map_or(raw, |(value, _)| value.trim_end())
        };
        if raw.is_empty() || raw.starts_with(['|', '>', '&', '*', '!']) {
            return None;
        }
        let value = if ((raw.starts_with('\"') && raw.ends_with('\"'))
            || (raw.starts_with('\'') && raw.ends_with('\'')))
            && raw.len() > 1
        {
            let inner = &raw[1..raw.len() - 1];
            if raw.starts_with('\"') && inner.contains('\\') {
                return None;
            }
            serde_yaml::Value::String(inner.to_owned())
        } else if raw.starts_with(['[', '{']) {
            return None;
        } else {
            match raw {
                "true" => serde_yaml::Value::Bool(true),
                "false" => serde_yaml::Value::Bool(false),
                "null" | "~" => serde_yaml::Value::Null,
                _ => {
                    // Native fast path recognizes only optional '-' plus an
                    // integer or decimal with digits after the decimal point.
                    let number = raw.strip_prefix('-').unwrap_or(raw);
                    let numeric = if let Some((whole, fraction)) = number.split_once('.') {
                        !fraction.is_empty()
                            && whole.bytes().all(|c| c.is_ascii_digit())
                            && fraction.bytes().all(|c| c.is_ascii_digit())
                    } else {
                        !number.is_empty() && number.bytes().all(|c| c.is_ascii_digit())
                    };
                    if numeric {
                        serde_yaml::to_value(raw.parse::<f64>().ok()?).ok()?
                    } else {
                        serde_yaml::Value::String(raw.to_owned())
                    }
                }
            }
        };
        result.insert(serde_yaml::Value::String(key.to_owned()), value);
    }
    Some(serde_yaml::Value::Mapping(result))
}

fn workbuddy_text(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::Null => None,
        serde_yaml::Value::String(value) => Some(value.clone()),
        serde_yaml::Value::Bool(value) => Some(value.to_string()),
        serde_yaml::Value::Number(value) => Some(if let Some(value) = value.as_f64() {
            workbuddy_number_text(value)
        } else {
            value.to_string()
        }),
        _ => workbuddy_json_text(value),
    }
}

fn workbuddy_number_text(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-Infinity"
        } else {
            "Infinity"
        }
        .to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    if value.abs() >= 1e21 || value.abs() < 1e-6 {
        let exponential = format!("{value:e}");
        let (mantissa, exponent) = exponential.split_once('e').unwrap();
        return format!(
            "{mantissa}e{}{exponent}",
            if exponent.starts_with('-') { "" } else { "+" }
        );
    }
    value.to_string()
}

fn workbuddy_json_text(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::Null => Some("null".to_owned()),
        serde_yaml::Value::String(value) => serde_json::to_string(value).ok(),
        serde_yaml::Value::Bool(value) => Some(value.to_string()),
        serde_yaml::Value::Number(value) => value.as_f64().map(|number| {
            if number.is_finite() {
                workbuddy_number_text(number)
            } else {
                "null".to_owned()
            }
        }),
        serde_yaml::Value::Sequence(values) => Some(format!(
            "[{}]",
            values
                .iter()
                .map(workbuddy_json_text)
                .collect::<Option<Vec<_>>>()?
                .join(",")
        )),
        serde_yaml::Value::Mapping(values) => Some(format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| Some(format!(
                    "{}:{}",
                    serde_json::to_string(&workbuddy_text(key)?).ok()?,
                    workbuddy_json_text(value)?
                )))
                .collect::<Option<Vec<_>>>()?
                .join(",")
        )),
        serde_yaml::Value::Tagged(_) => None,
    }
}

fn frontmatter(metadata: &SkillMetadata) -> serde_yaml::Value {
    fs::read_to_string(&metadata.source_path)
        .ok()
        .and_then(|content| workbuddy_document(&content).0.ok())
        .unwrap_or_default()
}

fn workbuddy_override_key(dir: &Path, metadata: &SkillMetadata) -> String {
    // Installed display names do not change the scanner's overrideKey.
    workbuddy_text(&frontmatter(metadata)["name"])
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| {
            dir.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
}

fn workbuddy_disabled(metadata: &SkillMetadata) -> bool {
    let Ok(content) = fs::read_to_string(&metadata.source_path) else {
        return false;
    };
    let (parsed, _) = workbuddy_document(&content);
    match parsed {
        Ok(fm) => fm["disable"].as_bool() == Some(true),
        Err(_) => {
            // readDisableFromFrontmatterLine: boolean text only, no inline
            // comments or quotes, case insensitive, key starts at column zero.
            let rest = content
                .strip_prefix("---\r\n")
                .or_else(|| content.strip_prefix("---\n"));
            rest.and_then(|rest| rest.find("\n---").map(|end| &rest[..end]))
                .and_then(|fm| {
                    fm.lines().find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        (key.trim_end().eq_ignore_ascii_case("disable")
                            && !key.starts_with(char::is_whitespace))
                        .then_some(value.trim())
                    })
                })
                .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        }
    }
}

fn workbuddy_visibility(
    dir: &Path,
    metadata: &SkillMetadata,
    settings_path: &Path,
) -> SkillVisibility {
    let fm = frontmatter(metadata);
    // Native userSettings entries call resolveOverrideKeyFromFile(file, userSkillsDir).
    // Unlike project/connector scan entries, unnamed nested user skills use a
    // colon-separated relative directory identity. Use lexical roots as native does.
    let key = if workbuddy_text(&fm["name"]).is_some_and(|name| !name.trim().is_empty()) {
        workbuddy_override_key(dir, metadata)
    } else {
        settings_path
            .parent()
            .and_then(|config| {
                let relative = dir.strip_prefix(config.join("skills")).ok()?;
                let parts = relative
                    .components()
                    .map(|part| match part {
                        std::path::Component::Normal(name) => {
                            Some(name.to_string_lossy().into_owned())
                        }
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()?;
                (!parts.is_empty()).then(|| parts.join(":"))
            })
            .unwrap_or_else(|| workbuddy_override_key(dir, metadata))
    };
    let overridden = fs::read_to_string(settings_path)
        .ok()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .and_then(|settings| {
            settings
                .get("skillOverrides")?
                .as_object()?
                .get(&key)?
                .as_str()
                .map(str::to_owned)
        });
    // The native list assigns (rather than ORs) disableModelInvocation when an
    // override string exists. Even an unrecognized string overrides FM true.
    let manual = match overridden.as_deref() {
        Some(value) => matches!(value, "user-invocable-only" | "off" | "name-only"),
        None => fm.get("disable-model-invocation").and_then(|v| v.as_bool()) == Some(true),
    };
    // user-invocable is exposed separately; false does not disable model invocation.
    if manual {
        SkillVisibility::ManualOnly
    } else {
        SkillVisibility::Automatic
    }
}

impl AgentSkillPolicy for WorkBuddyPolicy {
    fn agent(&self) -> AgentKind {
        AgentKind::WorkBuddy
    }

    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
        let mut roots = Vec::new();
        if !is_global_context(context) {
            if let Ok(root) = crate::agents::installation_root(&context.cwd, crate::InstallTarget::WorkBuddy) {
                roots.push(SkillRoot::new(root, SkillScope::Local));
            }
        }
        if let Ok(config) = crate::agents::workbuddy_config_root(&context.home) {
            roots.push(SkillRoot::new(config.join("skills"), SkillScope::User));
            roots.push(SkillRoot::new(config.join("connectors/skills"), SkillScope::User));
        }
        roots
    }

    fn is_enabled(&self, _: &Path, metadata: &SkillMetadata, _: &DiscoveryContext) -> bool {
        !workbuddy_disabled(metadata)
    }

    fn name_collision(&self) -> NameCollision {
        // Native project > user > connector, allowing duplicates within a source.
        NameCollision::FirstRootWins
    }

    fn visibility(
        &self,
        dir: &Path,
        metadata: &SkillMetadata,
        context: &DiscoveryContext,
    ) -> SkillVisibility {
        if let Ok(config) = crate::agents::workbuddy_config_root(&context.home) {
            workbuddy_visibility(dir, metadata, &config.join("settings.json"))
        } else if frontmatter(metadata)["disable-model-invocation"].as_bool() == Some(true) {
            SkillVisibility::ManualOnly
        } else {
            SkillVisibility::Automatic
        }
    }

    fn user_invocable(&self, _: &Path, metadata: &SkillMetadata, _: &DiscoveryContext) -> bool {
        frontmatter(metadata)["user-invocable"]
            .as_bool()
            .unwrap_or(true)
    }

    fn prompt_path_for_entry(
        &self,
        _: &SkillRoot,
        _: &Path,
        file: &Path,
        _: &SkillMetadata,
        _: &DiscoveryContext,
    ) -> Option<String> {
        Some(file.display().to_string())
    }

    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        // Static estimate; no claim that this is the desktop's exact prompt.
        render_name_description_catalog(skills)
    }

    fn estimate_tokens(&self, rendered: &str) -> usize {
        approx_token_count(rendered)
    }
}

impl AgentSkillPolicy for TraePolicy {
    fn agent(&self) -> AgentKind {
        AgentKind::Trae
    }

    fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
        let mut roots = Vec::new();
        if !is_global_context(context) {
            for suffix in [".trae/skills", ".agents/skills"] {
                roots.push(SkillRoot::new(context.cwd.join(suffix), SkillScope::Local));
            }
        }
        roots.push(SkillRoot::new(
            context.home.join(".trae-cn/skills"),
            SkillScope::User,
        ));
        roots
    }

    fn is_enabled(&self, _: &Path, metadata: &SkillMetadata, _: &DiscoveryContext) -> bool {
        let Ok((name, description)) = crate::library::read_frontmatter(&metadata.source_path)
        else {
            return false;
        };
        name.is_some_and(|name| !name.trim().is_empty())
            && description.is_some_and(|description| !description.trim().is_empty())
            && fs::read_to_string(&metadata.source_path)
                .is_ok_and(|content| !first_body_paragraph(&content).is_empty())
    }

    fn name_collision(&self) -> NameCollision {
        NameCollision::FirstWins
    }

    fn visibility(&self, _: &Path, _: &SkillMetadata, _: &DiscoveryContext) -> SkillVisibility {
        // CN docs confirm automatic relevance matching, but do not establish
        // disable-model-invocation, user-invocable, or an on-disk toggle schema.
        SkillVisibility::Automatic
    }

    fn prompt_path_for_entry(
        &self,
        _: &SkillRoot,
        _: &Path,
        file: &Path,
        _: &SkillMetadata,
        _: &DiscoveryContext,
    ) -> Option<String> {
        Some(file.display().to_string())
    }

    fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
        render_name_description_catalog(skills)
    }

    fn estimate_tokens(&self, rendered: &str) -> usize {
        approx_token_count(rendered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fixture() -> (tempfile::TempDir, DiscoveryContext) {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let cwd = temp.path().join("project");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        let context = DiscoveryContext {
            home,
            repository_root: Some(cwd.clone()),
            cwd,
        };
        (temp, context)
    }

    fn skill(dir: &Path, name: &str, fields: &str) -> SkillMetadata {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Use {name}\n{fields}---\nInstructions.\n"),
        )
        .unwrap();
        read_metadata_with_profile(&dir.join("SKILL.md"), MetadataProfile::BodyFallback).unwrap()
    }

    // Fixture roots are explicit: tests never inherit an actual desktop's
    // WORKBUDDY_CONFIG_DIR or settings, and never mutate process-wide env.
    struct WorkBuddyFixturePolicy;
    impl AgentSkillPolicy for WorkBuddyFixturePolicy {
        fn agent(&self) -> AgentKind {
            AgentKind::WorkBuddy
        }
        fn roots(&self, context: &DiscoveryContext) -> Vec<SkillRoot> {
            vec![
                SkillRoot::new(context.cwd.join(".workbuddy/skills"), SkillScope::Local),
                SkillRoot::new(context.home.join(".workbuddy/skills"), SkillScope::User),
                SkillRoot::new(
                    context.home.join(".workbuddy/connectors/skills"),
                    SkillScope::User,
                ),
            ]
        }
        fn is_enabled(
            &self,
            dir: &Path,
            metadata: &SkillMetadata,
            context: &DiscoveryContext,
        ) -> bool {
            WorkBuddyPolicy.is_enabled(dir, metadata, context)
        }
        fn name_collision(&self) -> NameCollision {
            WorkBuddyPolicy.name_collision()
        }
        fn visibility(
            &self,
            dir: &Path,
            metadata: &SkillMetadata,
            context: &DiscoveryContext,
        ) -> SkillVisibility {
            workbuddy_visibility(
                dir,
                metadata,
                &context.home.join(".workbuddy/settings.json"),
            )
        }
        fn user_invocable(
            &self,
            dir: &Path,
            metadata: &SkillMetadata,
            context: &DiscoveryContext,
        ) -> bool {
            WorkBuddyPolicy.user_invocable(dir, metadata, context)
        }
        fn render_visible_metadata(&self, skills: &[EffectiveSkill]) -> String {
            WorkBuddyPolicy.render_visible_metadata(skills)
        }
        fn estimate_tokens(&self, rendered: &str) -> usize {
            WorkBuddyPolicy.estimate_tokens(rendered)
        }
    }

    fn scan(policy: &dyn AgentSkillPolicy, context: &DiscoveryContext) -> Vec<EffectiveSkill> {
        let workbuddy = policy.agent() == AgentKind::WorkBuddy;
        discover(
            policy,
            context,
            if workbuddy {
                scanner::ScanProfile::WorkBuddy
            } else {
                scanner::ScanProfile::Recursive {
                    max_depth: 5,
                    max_directories: 2_000,
                    max_entries: 20_000,
                }
            },
            if workbuddy {
                MetadataProfile::WorkBuddy
            } else {
                MetadataProfile::StrictFrontmatter
            },
        )
    }

    #[test]
    fn workbuddy_config_and_project_folder_resolution() {
        let temp = tempdir().unwrap();
        let home = temp.path();
        let configured = home.join("custom config");
        assert_eq!(crate::agents::workbuddy_config_root_from(home, configured.to_str(), Some("2")).unwrap(), configured);
        assert_eq!(crate::agents::workbuddy_config_root_from(home, None, Some("2")).unwrap(), home.join(".workbuddy-2"));
        for blank in ["", "  "] {
            assert_eq!(crate::agents::workbuddy_config_root_from(home, Some(blank), None).unwrap(), home.join(".workbuddy"));
        }
        for invalid in ["relative", " relative "] {
            assert!(crate::agents::workbuddy_config_root_from(home, Some(invalid), None).is_err());
        }
        assert_eq!(crate::agents::workbuddy_project_folder(Some(".workbuddy-test")).unwrap(), ".workbuddy-test");
        for invalid in ["", "..", ".", "../outside", "/outside", "a/b", "a\\b"] {
            assert!(crate::agents::workbuddy_project_folder(Some(invalid)).is_err());
        }
    }

    #[test]
    fn workbuddy_real_boolean_fields_and_override_replacement() {
        let (_temp, context) = fixture();
        let dir = context.cwd.join("fixture");
        let settings = context.cwd.join("settings.json");
        let metadata = skill(
            &dir,
            "fixture",
            "disable-model-invocation: true\nuser-invocable: false\n",
        );
        assert_eq!(
            workbuddy_visibility(&dir, &metadata, &settings),
            SkillVisibility::ManualOnly
        );
        for value in ["user-invocable-only", "off", "name-only", "on", "unknown"] {
            fs::write(
                &settings,
                serde_json::json!({"skillOverrides":{"fixture":value}}).to_string(),
            )
            .unwrap();
            let expected = if matches!(value, "on" | "unknown") {
                SkillVisibility::Automatic
            } else {
                SkillVisibility::ManualOnly
            };
            assert_eq!(workbuddy_visibility(&dir, &metadata, &settings), expected);
        }
        fs::write(&settings, r#"{"skillOverrides":{"fixture":false}}"#).unwrap();
        assert_eq!(
            workbuddy_visibility(&dir, &metadata, &settings),
            SkillVisibility::ManualOnly
        );
        let metadata = skill(&dir, "fixture", "disable: true\n");
        assert!(!WorkBuddyPolicy.is_enabled(&dir, &metadata, &context));
        let metadata = skill(
            &dir,
            "fixture",
            "disable: 'true'\ndisable-model-invocation: 'true'\nuser-invocable: false\n",
        );
        assert!(WorkBuddyPolicy.is_enabled(&dir, &metadata, &context));
        assert_eq!(
            workbuddy_visibility(&dir, &metadata, &settings),
            SkillVisibility::Automatic
        );
    }

    #[test]
    fn workbuddy_nested_override_uses_scanned_name_not_relative_mutation_key() {
        let (_temp, context) = fixture();
        let dir = context.cwd.join("group/nested");
        let settings = context.cwd.join("settings.json");
        let metadata = skill(&dir, "real-name", "");
        fs::write(
            &settings,
            r#"{"skillOverrides":{"group:nested":"off","real-name":"on"}}"#,
        )
        .unwrap();
        assert_eq!(
            workbuddy_visibility(&dir, &metadata, &settings),
            SkillVisibility::Automatic
        );
        fs::write(
            dir.join("SKILL.md"),
            "---\ndescription: nested\n---\nInstructions.\n",
        )
        .unwrap();
        assert_eq!(
            workbuddy_override_key(
                &dir,
                &read_metadata_with_profile(&dir.join("SKILL.md"), MetadataProfile::BodyFallback)
                    .unwrap()
            ),
            "nested"
        );
    }

    #[test]
    fn trae_cn_roots_and_unverified_flags() {
        let (_temp, context) = fixture();
        let roots = TraePolicy.roots(&context);
        assert_eq!(
            roots
                .iter()
                .map(|root| root.path.clone())
                .collect::<Vec<_>>(),
            vec![
                context.cwd.join(".trae/skills"),
                context.cwd.join(".agents/skills"),
                context.home.join(".trae-cn/skills"),
            ]
        );
        let dir = context.cwd.join(".trae/skills/flags");
        let metadata = skill(
            &dir,
            "flags",
            "disable: true\ndisable-model-invocation: true\nuser-invocable: false\n",
        );
        assert!(TraePolicy.is_enabled(&dir, &metadata, &context));
        assert_eq!(
            TraePolicy.visibility(&dir, &metadata, &context),
            SkillVisibility::Automatic
        );
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: flags\ndescription: flags\n---\n",
        )
        .unwrap();
        assert!(!TraePolicy.is_enabled(&dir, &metadata, &context));
    }

    #[test]
    fn trae_keeps_shadowed_source_and_prefers_native_project_root() {
        let (_temp, context) = fixture();
        skill(&context.cwd.join(".trae/skills/same"), "same", "");
        skill(&context.cwd.join(".agents/skills/same"), "same", "");
        let skills = scan(&TraePolicy, &context);
        assert_eq!(skills.len(), 2, "the core must retain the shadowed source");
        let active = skills
            .iter()
            .filter(|s| s.visibility == SkillVisibility::Automatic)
            .collect::<Vec<_>>();
        assert_eq!(active.len(), 1);
        assert!(active[0].path.starts_with(context.cwd.join(".trae/skills")));
        assert_ne!(skills[0].path, skills[1].path);
        assert_eq!(
            skills
                .iter()
                .filter(|s| s.visibility == SkillVisibility::Shadowed)
                .count(),
            1
        );
    }

    #[test]
    fn workbuddy_same_source_keeps_distinct_same_name_files_active() {
        let (_temp, context) = fixture();
        let root = context.cwd.join(".workbuddy/skills");
        skill(&root.join("first"), "same", "");
        skill(&root.join("second"), "same", "");
        let skills = scan(&WorkBuddyFixturePolicy, &context);
        assert_eq!(
            skills
                .iter()
                .filter(|s| s.name == "same" && s.visibility == SkillVisibility::Automatic)
                .count(),
            2,
            "WorkBuddy allows distinct files with the same name in one source"
        );
    }

    #[test]
    fn linked_source_dedup_update_and_uninstall_are_observed_on_rescan() {
        let (_temp, context) = fixture();
        for (policy, native_root, alias_root) in [
            (
                &TraePolicy as &dyn AgentSkillPolicy,
                context.cwd.join(".trae/skills"),
                context.cwd.join(".agents/skills"),
            ),
            (
                &WorkBuddyFixturePolicy as &dyn AgentSkillPolicy,
                context.home.join(".workbuddy/skills"),
                context.home.join(".workbuddy/connectors/skills"),
            ),
        ] {
            let source = context.home.join("library/shared");
            skill(&source, "shared", "");
            let native = native_root.join("shared");
            let alias = alias_root.join("shared");
            fs::create_dir_all(native.parent().unwrap()).unwrap();
            fs::create_dir_all(alias.parent().unwrap()).unwrap();
            crate::directory_link::create(&source, &native).unwrap();
            crate::directory_link::create(&source, &alias).unwrap();
            assert_eq!(
                scan(policy, &context)
                    .iter()
                    .filter(|s| s.name == "shared")
                    .count(),
                1
            );
            skill(&source, "updated", "");
            assert_eq!(scan(policy, &context)[0].name, "updated");
            crate::directory_link::remove(
                &native,
                crate::directory_link::inspect(&native).unwrap().unwrap(),
            )
            .unwrap();
            crate::directory_link::remove(
                &alias,
                crate::directory_link::inspect(&alias).unwrap().unwrap(),
            )
            .unwrap();
            assert!(scan(policy, &context).is_empty());
            assert!(source.join("SKILL.md").is_file());
        }
    }
    #[test]
    fn workbuddy_metadata_coerces_text_and_preserves_fast_scalar_rules() {
        let (_temp, context) = fixture();
        let dir = context.cwd.join("metadata");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SKILL.md");
        for (fields, name, description, when) in [
            (
                "name: 42\ndescription: false\nwhen-to-use: true\n",
                "42",
                "false",
                Some("true"),
            ),
            (
                "name: 01\ndescription: 1.50\nwhen-to-use: null\n",
                "1",
                "1.5",
                None,
            ),
            ("name: 1e3\ndescription: TRUE\n", "1e3", "TRUE", None),
            (
                "name: [one, two]\ndescription: {answer: 42.0}\nwhen-to-use: [a, b]\n",
                "[\"one\",\"two\"]",
                "{\"answer\":42}",
                Some("[\"a\",\"b\"]"),
            ),
        ] {
            fs::write(&path, format!("---\n{fields}---\nBody.")).unwrap();
            let metadata = workbuddy_metadata(&path).unwrap();
            assert_eq!(metadata.name, name);
            assert_eq!(metadata.description, description);
            assert_eq!(metadata.when_to_use.as_deref(), when);
            assert!(metadata.has_explicit_description);
        }
        assert_eq!(workbuddy_number_text(1e21), "1e+21");
        assert_eq!(workbuddy_number_text(1e-7), "1e-7");
    }

    #[test]
    fn workbuddy_metadata_body_fallback_uses_utf16_and_retains_paragraphs() {
        let (_temp, context) = fixture();
        let dir = context.cwd.join("fallback");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SKILL.md");
        fs::write(&path, "  First paragraph.\n\nSecond paragraph.  ").unwrap();
        let metadata = workbuddy_metadata(&path).unwrap();
        assert_eq!(metadata.name, "fallback");
        assert_eq!(
            metadata.description,
            "First paragraph.\n\nSecond paragraph."
        );
        assert!(!metadata.has_explicit_description);
        fs::write(
            &path,
            format!("---\nname: unicode\n---\n{}", "😀".repeat(60)),
        )
        .unwrap();
        assert_eq!(
            workbuddy_metadata(&path).unwrap().description,
            "😀".repeat(50)
        );
        fs::write(&path, format!("{}a😀", "😀".repeat(49))).unwrap();
        assert_eq!(
            workbuddy_metadata(&path).unwrap().description,
            format!("{}a�", "😀".repeat(49))
        );
        fs::write(&path, "---\r\nname: crlf\r\ndescription: ''\r\n---\r\n").unwrap();
        let metadata = workbuddy_metadata(&path).unwrap();
        assert_eq!(metadata.name, "crlf");
        assert_eq!(metadata.description, "fallback");
        fs::write(&path, "---\nname: eof\n---").unwrap();
        assert_eq!(workbuddy_metadata(&path).unwrap().name, "eof");
        assert!(workbuddy_metadata(&dir.join("missing.md")).is_none());
    }

    #[test]
    fn workbuddy_installed_name_precedence_does_not_override_description_or_override_key() {
        let (_temp, context) = fixture();
        let dir = context.cwd.join("installed");
        skill(&dir, "front-name", "");
        let path = dir.join("SKILL.md");
        let files = [
            "_skillhub_meta.json",
            "_marketplace_meta.json",
            "_knot_meta.json",
            "_builtin_market_meta.json",
            "_user_meta.json",
        ];
        for (index, file) in files.iter().enumerate() {
            fs::write(dir.join(file), serde_json::json!({"name":format!("installed-{index}"),"description":"not the base description"}).to_string()).unwrap();
        }
        let metadata = workbuddy_metadata(&path).unwrap();
        assert_eq!(metadata.name, "installed-0");
        assert_eq!(metadata.description, "Use front-name");
        assert_eq!(workbuddy_override_key(&dir, &metadata), "front-name");
        for index in 0..files.len() - 1 {
            fs::remove_file(dir.join(files[index])).unwrap();
            assert_eq!(
                workbuddy_metadata(&path).unwrap().name,
                format!("installed-{}", index + 1)
            );
        }
        fs::write(dir.join(files[0]), "invalid JSON").unwrap();
        assert_eq!(workbuddy_metadata(&path).unwrap().name, "front-name");
        fs::write(dir.join(files[0]), "null").unwrap();
        assert_eq!(workbuddy_metadata(&path).unwrap().name, "front-name");
        fs::write(dir.join(files[0]), r#"{"name":""}"#).unwrap();
        assert_eq!(workbuddy_metadata(&path).unwrap().name, "front-name");
    }

    #[test]
    fn workbuddy_malformed_yaml_retains_metadata_and_disable_fallback() {
        let (_temp, context) = fixture();
        let dir = context.cwd.join("broken");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SKILL.md");
        fs::write(
            &path,
            "---\nname: [broken\ndisable: TRUE\nuser-invocable: false\n---\nBody",
        )
        .unwrap();
        fs::write(dir.join("_user_meta.json"), r#"{"name":"installed"}"#).unwrap();
        let metadata = workbuddy_metadata(&path).unwrap();
        assert_eq!(metadata.name, "installed");
        assert_eq!(metadata.description, "broken");
        assert!(!WorkBuddyPolicy.is_enabled(&dir, &metadata, &context));
        assert!(WorkBuddyPolicy.user_invocable(&dir, &metadata, &context));
        assert_eq!(workbuddy_override_key(&dir, &metadata), "broken");
    }

    #[test]
    fn workbuddy_user_invocable_accepts_only_boolean_and_trae_does_not_infer_it() {
        let (_temp, context) = fixture();
        let dir = context.cwd.join("picker");
        for (field, expected) in [
            ("user-invocable: false\n", false),
            ("user-invocable: true\n", true),
            ("user-invocable: 'false'\n", true),
            ("user-invocable: 0\n", true),
            ("", true),
        ] {
            skill(&dir, "picker", field);
            let metadata = workbuddy_metadata(&dir.join("SKILL.md")).unwrap();
            assert_eq!(
                WorkBuddyPolicy.user_invocable(&dir, &metadata, &context),
                expected
            );
            assert!(TraePolicy.user_invocable(&dir, &metadata, &context));
        }
    }

    #[test]
    fn workbuddy_unnamed_nested_user_override_uses_relative_identity() {
        let (_temp, context) = fixture();
        let config = context.home.join(".workbuddy");
        let dir = config.join("skills/group/nested");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), "Instructions.").unwrap();
        let metadata = workbuddy_metadata(&dir.join("SKILL.md")).unwrap();
        let settings = config.join("settings.json");
        fs::write(
            &settings,
            r#"{"skillOverrides":{"group:nested":"off","nested":"on"}}"#,
        )
        .unwrap();
        assert_eq!(
            workbuddy_visibility(&dir, &metadata, &settings),
            SkillVisibility::ManualOnly
        );
        let project = context.cwd.join(".workbuddy/skills/group/nested");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("SKILL.md"), "Instructions.").unwrap();
        let metadata = workbuddy_metadata(&project.join("SKILL.md")).unwrap();
        assert_eq!(
            workbuddy_visibility(&project, &metadata, &settings),
            SkillVisibility::Automatic
        );
    }

    #[test]
    fn workbuddy_first_root_wins_retains_shadowed_origins_and_picker_axis() {
        let (_temp, context) = fixture();
        let project = context.cwd.join(".workbuddy/skills");
        let user = context.home.join(".workbuddy/skills");
        skill(&project.join("one"), "same", "user-invocable: false\n");
        skill(&project.join("two"), "same", "");
        skill(&user.join("one"), "same", "");
        let skills = scan(&WorkBuddyFixturePolicy, &context);
        assert_eq!(skills.len(), 3);
        assert_eq!(
            skills
                .iter()
                .filter(|s| s.visibility == SkillVisibility::Automatic)
                .count(),
            2
        );
        assert_eq!(
            skills
                .iter()
                .filter(|s| s.visibility == SkillVisibility::Shadowed && s.path.starts_with(&user))
                .count(),
            1
        );
        assert!(
            !skills
                .iter()
                .find(|s| s.path.starts_with(project.join("one")))
                .unwrap()
                .user_invocable
        );
    }
}
