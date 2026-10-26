//! Pure, byte-preserving Hermes `plugins.enabled` edits.
//!
//! yaml-edit 0.3.2 supplies the CST and byte ranges; serde_yaml independently
//! validates the complete tree before and after each edit. No config is ever
//! serialized wholesale. Unsupported or ambiguous input has one redacted error.
use anyhow::{Result, anyhow, ensure};
use serde_yaml::{Mapping as Values, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, ops::Range};
use yaml_edit::{AsYaml, Mapping, Sequence, SyntaxKind as K, YamlFile, YamlNode};

const INVALID: &str = "Hermes YAML 配置冲突或结构不受支持，已保留";
const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
const MARKER: &str = "# Pleat managed Hermes plugins.enabled v1 ";
const RESERVED: &str = "Pleat managed Hermes";

struct Config {
    file: YamlFile,
    value: Value,
    root: Option<Mapping>,
    plugins: Option<Mapping>,
    enabled: Option<Sequence>,
    // Each range contains exactly a generated item, its marker and separator.
    owned: Vec<(String, Range<usize>)>,
}

fn invalid() -> anyhow::Error {
    anyhow!(INVALID)
}
fn key(name: &str) -> Value {
    Value::String(name.into())
}
fn semantic(text: &str) -> Result<Value> {
    serde_yaml::from_str(text).map_err(|_| invalid())
}
fn quote(plugin: &str) -> Result<String> {
    ensure!(
        !plugin.is_empty() && plugin.len() <= 4096 && !plugin.chars().any(char::is_control),
        INVALID
    );
    serde_json::to_string(plugin).map_err(|_| invalid())
}
fn marker(plugin: &str, flow: bool) -> String {
    format!(
        "{MARKER}{:x}:{}",
        Sha256::digest(plugin.as_bytes()),
        if flow { "flow" } else { "block" }
    )
}
fn span(node: &impl AsYaml) -> Result<Range<usize>> {
    let r = node.as_node().ok_or_else(invalid)?.text_range();
    Ok(usize::from(r.start())..usize::from(r.end()))
}
fn initial_spaces(text: &str, at: usize) -> usize {
    let line = text[..at].rfind('\n').map_or(0, |p| p + 1);
    text[line..at].bytes().take_while(|b| *b == b' ').count()
}
fn block_indent(text: &str, at: usize) -> Result<String> {
    let line = text[..at].rfind('\n').map_or(0, |p| p + 1);
    ensure!(text[line..at].bytes().all(|b| b == b' '), INVALID);
    Ok(text[line..at].into())
}
fn newline(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}
fn insert(text: &str, at: usize, addition: &str) -> String {
    let mut out = String::with_capacity(text.len() + addition.len());
    out.push_str(&text[..at]);
    out.push_str(addition);
    out.push_str(&text[at..]);
    out
}

// Use the library lexer, never a second hand-written YAML tokenizer. Bound flow
// nesting before recursive parsers. yaml-edit itself caps collection depth at
// 256; the complete CST is additionally limited to 64 levels below.
fn preflight(text: &str) -> Result<bool> {
    ensure!(text.len() <= MAX_BYTES, INVALID);
    let mut flow = 0usize;
    let mut empty = true;
    let mut line_structures = 0usize;
    for (kind, raw) in yaml_edit::lex(text) {
        ensure!(
            !matches!(
                kind,
                K::ANCHOR
                    | K::REFERENCE
                    | K::ALIAS
                    | K::TAG
                    | K::TAGGED_NODE
                    | K::DIRECTIVE
                    | K::BOM
            ),
            INVALID
        );
        match kind {
            K::LEFT_BRACE | K::LEFT_BRACKET => {
                flow += 1;
                ensure!(flow <= MAX_DEPTH, INVALID);
            }
            K::RIGHT_BRACE | K::RIGHT_BRACKET => {
                flow = flow.checked_sub(1).ok_or_else(invalid)?;
            }
            K::INDENT => ensure!(raw.len() <= 128, INVALID),
            K::DASH | K::COLON | K::QUESTION => {
                line_structures += 1;
                ensure!(line_structures <= 128, INVALID);
            }
            K::NEWLINE => line_structures = 0,
            _ => {}
        }
        if !matches!(
            kind,
            K::WHITESPACE | K::INDENT | K::NEWLINE | K::COMMENT | K::EOF
        ) {
            empty = false;
        }
    }
    ensure!(flow == 0, INVALID);
    Ok(empty)
}

// Compare the CST's actual entries (not a permissive key lookup) to serde's
// complete value. This detects skipped CST nodes, duplicate keys, unsupported
// complex keys and disagreement over quoted/unicode scalars.
fn agree(node: &YamlNode, expected: &Value, depth: usize) -> Result<()> {
    ensure!(depth <= MAX_DEPTH, INVALID);
    match node {
        YamlNode::Scalar(s) => ensure!(semantic(&s.to_string())? == *expected, INVALID),
        YamlNode::Mapping(m) => {
            let values = expected.as_mapping().ok_or_else(invalid)?;
            let mut seen = Values::new();
            for entry in m.entries() {
                let k = entry.key_node().ok_or_else(invalid)?;
                ensure!(k.as_scalar().is_some(), INVALID);
                let k = semantic(&k.to_string())?;
                ensure!(k != key("<<") && !seen.contains_key(&k), INVALID);
                let v = values.get(&k).ok_or_else(invalid)?;
                agree(&entry.value_node().ok_or_else(invalid)?, v, depth + 1)?;
                seen.insert(k, Value::Null);
            }
            ensure!(seen.len() == values.len(), INVALID);
        }
        YamlNode::Sequence(s) => {
            let values = expected.as_sequence().ok_or_else(invalid)?;
            let nodes: Vec<_> = s.values().collect();
            ensure!(
                nodes.len() == values.len() && s.len() == values.len(),
                INVALID
            );
            for (n, v) in nodes.iter().zip(values) {
                agree(n, v, depth + 1)?;
            }
        }
        _ => return Err(invalid()),
    }
    Ok(())
}

fn field(mapping: &Mapping, name: &str) -> Result<Option<YamlNode>> {
    for entry in mapping.entries() {
        let k = entry.key_node().ok_or_else(invalid)?;
        if semantic(&k.to_string())? == key(name) {
            return entry.value_node().map(Some).ok_or_else(invalid);
        }
    }
    Ok(None)
}
fn names(value: Option<&Value>) -> Result<Vec<String>> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Sequence(v)) => v
            .iter()
            .map(|v| v.as_str().map(str::to_owned).ok_or_else(invalid))
            .collect(),
        _ => Err(invalid()),
    }
}
impl Config {
    fn parse(text: &str) -> Result<Self> {
        let empty = preflight(text)?;
        let parsed = YamlFile::parse(text);
        ensure!(!parsed.has_errors(), INVALID);
        let file = parsed.tree();
        ensure!(
            file.to_string() == text && file.documents().count() <= 1,
            INVALID
        );
        let value = semantic(text)?;
        let root = if empty {
            ensure!(value == Value::Null, INVALID);
            None
        } else {
            let root = file
                .document()
                .and_then(|d| d.as_mapping())
                .ok_or_else(invalid)?;
            agree(&YamlNode::Mapping(root.clone()), &value, 0)?;
            Some(root)
        };
        let plugins = root
            .as_ref()
            .map(|r| field(r, "plugins"))
            .transpose()?
            .flatten()
            .map(|p| p.as_mapping().cloned().ok_or_else(invalid))
            .transpose()?;
        let enabled = plugins
            .as_ref()
            .map(|p| field(p, "enabled"))
            .transpose()?
            .flatten()
            .map(|e| e.as_sequence().cloned().ok_or_else(invalid))
            .transpose()?;
        let mut config = Self {
            file,
            value,
            root,
            plugins,
            enabled,
            owned: Vec::new(),
        };
        // Both native lists must have understood string semantics, even when
        // only a different plugin is being edited. Unknown truthiness is unsafe.
        config.list("enabled")?;
        config.list("disabled")?;
        config.read_ownership(text)?;
        Ok(config)
    }
    fn list(&self, name: &str) -> Result<Vec<String>> {
        let plugins = self
            .value
            .as_mapping()
            .and_then(|r| r.get(&key("plugins")))
            .and_then(Value::as_mapping);
        names(plugins.and_then(|p| p.get(&key(name))))
    }
    fn read_ownership(&mut self, text: &str) -> Result<()> {
        let mut comments = BTreeMap::new();
        for comment in self.file.comments().filter(|c| c.text().contains(RESERVED)) {
            ensure!(
                comments
                    .insert(comment.text().to_owned(), comment.byte_range())
                    .is_none(),
                INVALID
            );
        }
        if comments.is_empty() {
            return Ok(());
        }
        let seq = self.enabled.as_ref().ok_or_else(invalid)?;
        let flow = seq.is_flow_style();
        let items: Vec<_> = seq.values().collect();
        let values = self.list("enabled")?;
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for plugin in &values {
            *counts.entry(plugin.clone()).or_default() += 1;
        }
        for (item, plugin) in items.iter().zip(&values) {
            let Some(cr) = comments.remove(&marker(plugin, flow)) else {
                continue;
            };
            ensure!(counts.get(plugin) == Some(&1), INVALID);
            let r = span(item)?;
            ensure!(
                text.get(r.clone()) == Some(quote(plugin)?.as_str()),
                INVALID
            );
            let between = if flow { ", " } else { " " };
            ensure!(
                cr.start as usize == r.end + between.len()
                    && text.get(r.end..cr.start as usize) == Some(between),
                INVALID
            );
            let end = cr.end as usize;
            let nl = newline(text);
            ensure!(text.get(end..end + nl.len()) == Some(nl), INVALID);
            let (start, indent) = if flow {
                (
                    r.start,
                    " ".repeat(initial_spaces(text, span(seq)?.start) + 2),
                )
            } else {
                let start = r.start.checked_sub(2).ok_or_else(invalid)?;
                ensure!(text.get(start..r.start) == Some("- "), INVALID);
                (start, block_indent(text, start)?)
            };
            // Our insertion carries the next item's indentation. Requiring
            // the exact suffix also rejects edited/reindented markers.
            let finish = end + nl.len() + indent.len();
            ensure!(
                text.get(end + nl.len()..finish) == Some(indent.as_str()),
                INVALID
            );
            self.owned.push((plugin.clone(), start..finish));
        }
        ensure!(comments.is_empty(), INVALID);
        Ok(())
    }
}

// Every inserted flow item owns a trailing comma (legal even for the last
// element). Prepending therefore needs no rewrite of a user's last item or
// comment, and removing any owned item never steals another item's comma.
fn flow_item(plugin: &str, indent: usize, nl: &str) -> Result<String> {
    Ok(format!(
        "{}, {}{nl}{}",
        quote(plugin)?,
        marker(plugin, true),
        " ".repeat(indent)
    ))
}
fn add_field(text: &str, mapping: &Mapping, name: &str, value: &str) -> Result<String> {
    let node = mapping.as_node().ok_or_else(invalid)?;
    if mapping.is_flow_style() {
        let at = node
            .children_with_tokens()
            .filter_map(|n| n.into_token())
            .find(|t| t.kind() == K::LEFT_BRACE)
            .ok_or_else(invalid)?
            .text_range()
            .end();
        Ok(insert(text, usize::from(at), &format!("{name}: {value}, ")))
    } else {
        let first = node
            .children()
            .find(|n| n.kind() == K::MAPPING_ENTRY)
            .ok_or_else(invalid)?;
        let at = usize::from(first.text_range().start());
        let indent = block_indent(text, at)?;
        Ok(insert(
            text,
            at,
            &format!("{name}: {value}{}{}", newline(text), indent),
        ))
    }
}

fn expected_enable(config: &Config, plugin: &str) -> Result<Value> {
    let mut value = config.value.clone();
    if value == Value::Null {
        value = Value::Mapping(Values::new());
    }
    let root = value.as_mapping_mut().ok_or_else(invalid)?;
    if !root.contains_key(&key("plugins")) {
        root.insert(key("plugins"), Value::Mapping(Values::new()));
    }
    let plugins = root
        .get_mut(&key("plugins"))
        .and_then(Value::as_mapping_mut)
        .ok_or_else(invalid)?;
    if !plugins.contains_key(&key("enabled")) {
        plugins.insert(key("enabled"), Value::Sequence(Vec::new()));
    }
    plugins
        .get_mut(&key("enabled"))
        .and_then(Value::as_sequence_mut)
        .ok_or_else(invalid)?
        .insert(0, key(plugin));
    Ok(value)
}
fn checked_output(text: String, expected: Value, plugin: &str, owned: bool) -> Result<String> {
    let after = Config::parse(&text)?;
    ensure!(
        after.value == expected && after.owned.iter().any(|(p, _)| p == plugin) == owned,
        INVALID
    );
    Ok(text)
}

/// Explicit native enable only. `plugins.disabled` wins, including when the
/// same plugin already appears in enabled. Existing user entries are not claimed.
pub(super) fn enable(text: &str, plugin: &str) -> Result<String> {
    quote(plugin)?;
    let config = Config::parse(text)?;
    ensure!(
        !config.list("disabled")?.iter().any(|p| p == plugin),
        INVALID
    );
    if config.list("enabled")?.iter().any(|p| p == plugin) {
        return Ok(text.into());
    }
    let nl = newline(text);
    let output = if let Some(seq) = &config.enabled {
        if seq.is_flow_style() {
            let node = seq.as_node().ok_or_else(invalid)?;
            let at = node
                .children_with_tokens()
                .filter_map(|n| n.into_token())
                .find(|t| t.kind() == K::LEFT_BRACKET)
                .ok_or_else(invalid)?
                .text_range()
                .end();
            let indent = initial_spaces(text, span(seq)?.start) + 2;
            insert(text, usize::from(at), &flow_item(plugin, indent, nl)?)
        } else {
            let node = seq.as_node().ok_or_else(invalid)?;
            let entry = node
                .children()
                .find(|n| n.kind() == K::SEQUENCE_ENTRY)
                .ok_or_else(invalid)?;
            let at = usize::from(entry.text_range().start());
            let indent = block_indent(text, at)?;
            insert(
                text,
                at,
                &format!("- {} {}{nl}{indent}", quote(plugin)?, marker(plugin, false)),
            )
        }
    } else {
        let mapping = config.plugins.as_ref().or(config.root.as_ref());
        // Use the original line's leading indentation, also for inline flow
        // parents. The closing delimiter remains safely inside the flow value.
        let indent = mapping
            .map(|m| span(m).map(|r| initial_spaces(text, r.start) + 2))
            .transpose()?
            .unwrap_or(2);
        let enabled = format!("[{}]", flow_item(plugin, indent, nl)?);
        if let Some(plugins) = &config.plugins {
            add_field(text, plugins, "enabled", &enabled)?
        } else if let Some(root) = &config.root {
            add_field(text, root, "plugins", &format!("{{enabled: {enabled}}}"))?
        } else {
            let separator = if text.is_empty() || text.ends_with('\n') {
                ""
            } else {
                nl
            };
            format!("{text}{separator}plugins: {{enabled: {enabled}}}{nl}")
        }
    };
    checked_output(output, expected_enable(&config, plugin)?, plugin, true)
}

/// Remove only the exact generated item. Unowned user entries and disabled
/// declarations survive unchanged. A moved/edited/detached marker is a conflict.
pub(super) fn disable_owned(text: &str, plugin: &str) -> Result<String> {
    quote(plugin)?;
    let config = Config::parse(text)?;
    let Some((_, range)) = config.owned.iter().find(|(p, _)| p == plugin) else {
        return Ok(text.into());
    };
    let mut expected = config.value.clone();
    let plugins = expected
        .as_mapping_mut()
        .and_then(|m| m.get_mut(&key("plugins")))
        .and_then(Value::as_mapping_mut)
        .ok_or_else(invalid)?;
    let enabled = plugins
        .get_mut(&key("enabled"))
        .and_then(Value::as_sequence_mut)
        .ok_or_else(invalid)?;
    let at = enabled
        .iter()
        .position(|v| v == &key(plugin))
        .ok_or_else(invalid)?;
    enabled.remove(at);
    // A user could have removed the original block items since installation.
    // Do not silently turn the field into YAML null when removing the last one.
    ensure!(
        config
            .enabled
            .as_ref()
            .is_none_or(|s| s.is_flow_style() || !enabled.is_empty()),
        INVALID
    );
    let mut output = text.to_owned();
    output.replace_range(range.clone(), "");
    checked_output(output, expected, plugin, false)
}

/// Ownership only, independent of a user disabled declaration. Malformed or
/// misplaced reserved markers return an error, never a guessed boolean.
pub(super) fn is_managed(text: &str, plugin: &str) -> Result<bool> {
    quote(plugin)?;
    Ok(Config::parse(text)?.owned.iter().any(|(p, _)| p == plugin))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flow_roundtrip_retains_comments_unicode_and_other_settings() {
        for text in [
            "{plugins: {enabled: ['用户', \"\\u0061\",], disabled: [other]}, secret: 'test-fixture'} # tail\n",
            "# header\nplugins:\n  enabled: [user # retain\n    , '用户',]\n  other: {answer: 42}\nrest: yes\n",
            "plugins: {enabled: []} # keep\n",
            "'plugins': {\"\\u0065nabled\": ['用户']} # quoted keys\n",
            "plugins: {enabled: [a]}\r\nrest: true\r\n",
        ] {
            let output = enable(text, "插件").unwrap();
            assert!(is_managed(&output, "插件").unwrap());
            assert_eq!(enable(&output, "插件").unwrap(), output);
            assert_eq!(disable_owned(&output, "插件").unwrap(), text);
        }
    }

    #[test]
    fn block_roundtrip_and_multiple_owners_are_independent() {
        let text = "plugins:\n  enabled: # retain header\n    - '用户' # retain item\n    - other\n  disabled: [blocked]\nnext: unchanged\n";
        let one = enable(text, "one").unwrap();
        let two = enable(&one, "two").unwrap();
        assert_eq!(disable_owned(&two, "two").unwrap(), one);
        let only_two = disable_owned(&two, "one").unwrap();
        assert!(is_managed(&only_two, "two").unwrap());
        assert_eq!(disable_owned(&only_two, "two").unwrap(), text);
        for original in ["plugins: {enabled: [user]}\n", "plugins: {enabled: []}\n"] {
            let both = enable(&enable(original, "one").unwrap(), "two").unwrap();
            let only_two = disable_owned(&both, "one").unwrap();
            assert!(is_managed(&only_two, "two").unwrap());
            assert_eq!(disable_owned(&only_two, "two").unwrap(), original);
        }
    }

    #[test]
    fn creates_only_the_native_enabled_path() {
        for text in [
            "",
            "# keep",
            "# keep\n",
            "{}\n",
            "other: true\n",
            "plugins: {}\n",
            "plugins:\n  disabled: [other]\n",
            "{plugins: {other: yes}, x: 1}\n",
        ] {
            let output = enable(text, "demo").unwrap();
            assert!(is_managed(&output, "demo").unwrap());
            let removed = disable_owned(&output, "demo").unwrap();
            assert!(!is_managed(&removed, "demo").unwrap());
            assert_eq!(
                semantic(&removed).unwrap()["plugins"]["enabled"],
                Value::Sequence(vec![])
            );
            assert_eq!(
                enable(&removed, "demo").unwrap().matches(RESERVED).count(),
                1
            );
        }
    }

    #[test]
    fn disabled_wins_and_user_entries_are_not_claimed() {
        for text in [
            "plugins: {disabled: [demo]}\n",
            "plugins: {disabled: [demo], enabled: [demo]}\n",
        ] {
            assert!(enable(text, "demo").is_err());
            assert_eq!(disable_owned(text, "demo").unwrap(), text);
        }
        let user = "plugins: {enabled: ['demo', demo]} # unowned\n";
        assert_eq!(enable(user, "demo").unwrap(), user);
        assert!(!is_managed(user, "demo").unwrap());
        assert_eq!(disable_owned(user, "demo").unwrap(), user);
        let owned = enable("plugins: {disabled: [other]}\n", "demo").unwrap();
        let blocked = owned.replace("[other]", "[other, demo]");
        assert!(enable(&blocked, "demo").is_err());
        assert!(
            disable_owned(&blocked, "demo")
                .unwrap()
                .contains("[other, demo]")
        );
    }

    #[test]
    fn malformed_yaml_unknown_shapes_and_moved_markers_are_redacted() {
        let owned = enable("plugins: {enabled: []}\n", "demo").unwrap();
        let m = marker("demo", true);
        let bad = [
            "plugins: {enabled: [\n".to_owned(),
            "plugins: {}\nplugins: {}\n".to_owned(),
            "plugins: {enabled: [], 'enabled': []}\n".to_owned(),
            "secret: {a: 1, a: 2}\n".to_owned(),
            "[]\n".to_owned(),
            "null\n".to_owned(),
            "plugins: null\n".to_owned(),
            "plugins: {enabled: null}\n".to_owned(),
            "plugins: {disabled: [false]}\n".to_owned(),
            "a: &x [secret]\nplugins: {enabled: *x}\n".to_owned(),
            "plugins: {<<: {enabled: []}}\n".to_owned(),
            "a: !unknown secret\n".to_owned(),
            "---\n{}\n---\n{}\n".to_owned(),
            owned.replace("\"demo\"", "\"changed\""),
            owned.replace("\"demo\"", "'demo'"),
            owned.replace("enabled:", "other:"),
            owned.replace(&m, &format!("{m} modified")),
            format!("plugins: {{enabled: [demo]}}\n{m}\n"),
            owned.replace("\"demo\",", "demo, demo,"),
        ];
        for text in bad {
            for error in [
                enable(&text, "demo").unwrap_err(),
                disable_owned(&text, "demo").unwrap_err(),
                is_managed(&text, "demo").unwrap_err(),
            ] {
                assert_eq!(format!("{error:#}"), INVALID);
            }
        }
    }

    #[test]
    fn rejects_size_depth_and_plugin_control_characters() {
        assert!(enable(&" ".repeat(MAX_BYTES + 1), "demo").is_err());
        assert!(enable(&format!("x: {}0{}", "[".repeat(65), "]".repeat(65)), "demo").is_err());
        for plugin in ["", "demo\nother", "demo\0"] {
            assert!(enable("", plugin).is_err());
        }
    }
}
