//! Byte-preserving, fail-closed edits of one OpenClaw JSON5 plugin entry.
//! Pure text only: the caller owns snapshot CAS, ACLs, publication and recovery.
//! Config ownership markers do not establish native/bundled plugin provenance.
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeSet, ops::Range};

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
const INVALID: &str = "OpenClaw JSON5 配置结构无效或不受支持，已保留";
const DUPLICATE: &str = "OpenClaw JSON5 配置字段重复，已保留";
const POLICY: &str = "OpenClaw 插件策略冲突，已保留";
const OWNERSHIP: &str = "OpenClaw 插件配置归属冲突，已保留";
const DYNAMIC: &str = "OpenClaw 插件策略含 include 或动态结构，已保留";

#[derive(Clone, Copy)]
struct Token {
    kind: u8,
    start: usize,
    end: usize,
}

// Bound both size and nesting BEFORE recursive parsing / json5 deserialization.
// ASCII identifier keys and ordinary JSON5 are supported. Escaped/unicode bare
// identifiers, nonfinite numbers and non-ASCII whitespace are conservative errors.
fn lex(text: &str) -> Result<Vec<Token>> {
    ensure!(
        text.len() <= MAX_BYTES,
        "OpenClaw JSON5 配置超过 2 MiB，已保留"
    );
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut stack = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let mut kind = bytes[i];
        match kind {
            b' ' | b'\t' | b'\r' | b'\n' => {
                i += 1;
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i += 2;
                while i < bytes.len() && !matches!(bytes[i], b'\r' | b'\n') {
                    i += 1;
                }
                // JSON5 treats these as line terminators; do not swallow syntax.
                ensure!(!text[start..i].contains(['\u{2028}', '\u{2029}']), INVALID);
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && &bytes[i..i + 2] != b"*/" {
                    i += 1;
                }
                ensure!(i + 1 < bytes.len(), INVALID);
                i += 2;
                continue;
            }
            b'"' | b'\'' => {
                i += 1;
                while i < bytes.len() && bytes[i] != kind {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                ensure!(i < bytes.len(), INVALID);
                i += 1;
            }
            b'{' | b'[' => {
                stack.push(kind);
                ensure!(
                    stack.len() <= MAX_DEPTH,
                    "OpenClaw JSON5 配置嵌套超过 64 层，已保留"
                );
                i += 1;
            }
            b'}' | b']' => {
                ensure!(
                    stack.pop() == Some(if kind == b'}' { b'{' } else { b'[' }),
                    INVALID
                );
                i += 1;
            }
            b',' | b':' => i += 1,
            _ => {
                kind = b'v';
                while i < bytes.len()
                    && !matches!(
                        bytes[i],
                        b' ' | b'\t'
                            | b'\r'
                            | b'\n'
                            | b'/'
                            | b'{'
                            | b'}'
                            | b'['
                            | b']'
                            | b','
                            | b':'
                            | b'"'
                            | b'\''
                    )
                {
                    ensure!(bytes[i].is_ascii(), INVALID);
                    i += 1;
                }
                ensure!(i > start, INVALID);
            }
        }
        tokens.push(Token {
            kind,
            start,
            end: i,
        });
    }
    ensure!(stack.is_empty(), INVALID);
    Ok(tokens)
}

struct Member {
    key: String,
    start: usize,
    value: Node,
}

enum Shape {
    Object(Vec<Member>),
    Array(Vec<Node>),
    Scalar,
}

struct Node {
    tokens: Range<usize>,
    shape: Shape,
}

impl Node {
    fn field(&self, key: &str) -> Option<&Node> {
        self.member(key).map(|m| &m.value)
    }

    fn member(&self, key: &str) -> Option<&Member> {
        match &self.shape {
            Shape::Object(members) => members.iter().find(|m| m.key == key),
            _ => None,
        }
    }

    fn last(&self) -> Option<&Node> {
        match &self.shape {
            Shape::Object(members) => members.last().map(|m| &m.value),
            Shape::Array(items) => items.last(),
            Shape::Scalar => None,
        }
    }
}

struct Parser<'a> {
    text: &'a str,
    tokens: &'a [Token],
    cursor: usize,
}

impl Parser<'_> {
    fn at(&self, kind: u8) -> bool {
        self.tokens.get(self.cursor).is_some_and(|t| t.kind == kind)
    }

    fn take(&mut self, kind: u8) -> Result<()> {
        ensure!(self.at(kind), INVALID);
        self.cursor += 1;
        Ok(())
    }

    fn key(&mut self) -> Result<String> {
        let token = self
            .tokens
            .get(self.cursor)
            .ok_or_else(|| anyhow::anyhow!(INVALID))?;
        let raw = &self.text[token.start..token.end];
        let key = match token.kind {
            b'"' | b'\'' => json5::from_str::<String>(raw).map_err(|_| anyhow::anyhow!(INVALID))?,
            b'v' => {
                let mut bytes = raw.bytes();
                ensure!(
                    bytes
                        .next()
                        .is_some_and(|b| b.is_ascii_alphabetic() || matches!(b, b'_' | b'$'))
                        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$')),
                    INVALID
                );
                raw.to_owned()
            }
            _ => bail!(INVALID),
        };
        self.cursor += 1;
        Ok(key)
    }

    fn value(&mut self) -> Result<Node> {
        let start = self.cursor;
        let token = *self
            .tokens
            .get(start)
            .ok_or_else(|| anyhow::anyhow!(INVALID))?;
        self.cursor += 1;
        let shape = match token.kind {
            b'{' => {
                let mut members = Vec::new();
                let mut keys = BTreeSet::new();
                while !self.at(b'}') {
                    let start = self.cursor;
                    let key = self.key()?;
                    ensure!(keys.insert(key.clone()), DUPLICATE);
                    self.take(b':')?;
                    let value = self.value()?;
                    members.push(Member { key, start, value });
                    if !self.at(b',') {
                        break;
                    }
                    self.cursor += 1;
                }
                self.take(b'}')?;
                Shape::Object(members)
            }
            b'[' => {
                let mut items = Vec::new();
                while !self.at(b']') {
                    items.push(self.value()?);
                    if !self.at(b',') {
                        break;
                    }
                    self.cursor += 1;
                }
                self.take(b']')?;
                Shape::Array(items)
            }
            b'"' | b'\'' => Shape::Scalar,
            b'v' => {
                let raw = &self.text[token.start..token.end];
                if !matches!(raw, "true" | "false" | "null") {
                    // Number rejects NaN/Infinity (serde_json::Value can turn them
                    // into null), preventing false semantic equivalence.
                    let _: serde_json::Number =
                        json5::from_str(raw).map_err(|_| anyhow::anyhow!(INVALID))?;
                }
                Shape::Scalar
            }
            _ => bail!(INVALID),
        };
        Ok(Node {
            tokens: start..self.cursor,
            shape,
        })
    }
}

struct Document {
    tokens: Vec<Token>,
    root: Node,
    value: Value,
}

fn parse(text: &str) -> Result<Document> {
    let tokens = lex(text)?;
    let mut parser = Parser {
        text,
        tokens: &tokens,
        cursor: 0,
    };
    let root = parser.value()?;
    ensure!(
        parser.cursor == tokens.len() && matches!(root.shape, Shape::Object(_)),
        INVALID
    );
    let value = json5::from_str::<Value>(text).map_err(|_| anyhow::anyhow!(INVALID))?;
    Ok(Document {
        tokens,
        root,
        value,
    })
}

fn object(value: &Value) -> Result<&serde_json::Map<String, Value>> {
    let map = value.as_object().ok_or_else(|| anyhow::anyhow!(INVALID))?;
    ensure!(!map.contains_key("$include"), DYNAMIC);
    Ok(map)
}

fn validate_id(id: &str) -> Result<()> {
    // IDs remain data, never source syntax. Bound expansion and JS special keys.
    ensure!(
        !policy_id(id).is_empty()
            && id.len() <= 256
            && id == id.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
            && !id.chars().any(char::is_control)
            && !id.contains("${")
            && !matches!(
                policy_id(id).as_str(),
                "$include" | "__proto__" | "constructor" | "prototype"
            ),
        "OpenClaw 插件 ID 无效，已保留"
    );
    Ok(())
}

fn policy_id(id: &str) -> String {
    // Native 2026.10.5 trims and lowercases allow/deny AND entries keys. An
    // equivalent spelling is an ownership ambiguity, never another free slot.
    id.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
        .to_lowercase()
}

// Only inspect paths that can change target policy. Includes/env in unrelated
// subtrees remain opaque original bytes; NEVER open include paths or read env.
fn policy(doc: &Document, id: &str) -> Result<()> {
    validate_id(id)?;
    let root = object(&doc.value)?;
    let Some(plugins) = root.get("plugins") else {
        return Ok(());
    };
    let plugins = object(plugins)?;
    if let Some(enabled) = plugins.get("enabled") {
        ensure!(enabled.is_boolean(), INVALID);
    }
    let target = policy_id(id);
    ensure!(!target.is_empty(), POLICY);
    if let Some(entries) = plugins.get("entries") {
        for (key, entry) in object(entries)? {
            ensure!(!key.contains("${"), DYNAMIC);
            ensure!(policy_id(key) != target || key == id, POLICY);
            ensure!(entry.is_object(), INVALID);
        }
    }
    for key in ["allow", "deny"] {
        if let Some(list) = plugins.get(key) {
            let list = list.as_array().ok_or_else(|| anyhow::anyhow!(DYNAMIC))?;
            for item in list {
                let item = item.as_str().ok_or_else(|| anyhow::anyhow!(DYNAMIC))?;
                // Also reject escaped templates: native substitution rewrites
                // $${VAR} to ${VAR}, so raw equality would not prove identity.
                ensure!(!item.contains("${"), DYNAMIC);
                ensure!(policy_id(item) != target || item == id, POLICY);
            }
            ensure!(
                list.iter().filter(|v| v.as_str() == Some(id)).count() <= 1,
                POLICY
            );
        }
    }
    Ok(())
}

fn owned(allow_added: bool) -> Value {
    json!({"enabled":true,
        "hooks":{"allowPromptInjection":true,"allowConversationAccess":true},
        "config":{"pleatManagedProfileVersion":1,"pleatManagedAllowAdded":allow_added}})
}

fn ownership(entry: &Value) -> Result<bool> {
    for allow_added in [false, true] {
        if entry == &owned(allow_added) {
            return Ok(allow_added);
        }
    }
    bail!(OWNERSHIP)
}

type Edit = (Range<usize>, String);

fn append(doc: &Document, container: &Node, body: String, edits: &mut Vec<Edit>) {
    let close = container.tokens.end - 1;
    let trailing = doc.tokens[close - 1].kind == b',';
    let at = doc.tokens[close].start;
    // Preserve an existing trailing comma across enable -> disable.
    let mut body = if trailing { format!("{body},") } else { body };
    if let Some(last) = container.last() {
        if !trailing {
            let separator = doc.tokens[last.tokens.end - 1].end;
            if separator == at {
                // Two insertions at the same offset reverse their text order
                // during right-to-left editing. Emit one unambiguous insertion.
                body.insert(0, ',');
            } else {
                edits.push((separator..separator, ",".to_owned()));
            }
        }
    }
    edits.push((at..at, body));
}

fn remove(doc: &Document, range: Range<usize>, edits: &mut Vec<Edit>) {
    let comma = if doc.tokens.get(range.end).is_some_and(|t| t.kind == b',') {
        Some(range.end)
    } else if range.start > 0 && doc.tokens[range.start - 1].kind == b',' {
        Some(range.start - 1)
    } else {
        None
    };
    // Remove ONLY tokens, preserving even comments/whitespace inside owned data.
    for i in range.chain(comma) {
        let t = doc.tokens[i];
        edits.push((t.start..t.end, String::new()));
    }
}

fn finish(text: &str, mut edits: Vec<Edit>, expected: Value) -> Result<String> {
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let added: usize = edits.iter().map(|(_, s)| s.len()).sum();
    let removed: usize = edits.iter().map(|(r, _)| r.len()).sum();
    ensure!(
        text.len() + added - removed <= MAX_BYTES,
        "OpenClaw JSON5 编辑结果超过 2 MiB，已保留"
    );
    let mut output = text.to_owned();
    let mut boundary = text.len();
    for (range, replacement) in edits {
        ensure!(range.end <= boundary, INVALID);
        boundary = range.start;
        output.replace_range(range, &replacement);
    }
    // Exact whole-document semantic equality against the explicitly authorized
    // delta also proves that EVERY non-owned field retained its previous value.
    ensure!(
        parse(&output)?.value == expected,
        "OpenClaw JSON5 编辑语义校验失败，已保留"
    );
    Ok(output)
}

pub(super) fn enable(text: &str, id: &str) -> Result<String> {
    let doc = parse(text)?;
    policy(&doc, id)?;
    let plugins = doc.value.get("plugins");
    if let Some(plugins) = plugins {
        ensure!(plugins.get("enabled") != Some(&Value::Bool(false)), POLICY);
        ensure!(
            !plugins
                .get("deny")
                .and_then(Value::as_array)
                .is_some_and(|items| items.iter().any(|v| v.as_str() == Some(id))),
            POLICY
        );
    }
    let allow = plugins
        .and_then(|p| p.get("allow"))
        .and_then(Value::as_array);
    let has_id = allow.is_some_and(|items| items.iter().any(|v| v.as_str() == Some(id)));
    if let Some(entry) = plugins
        .and_then(|p| p.get("entries"))
        .and_then(|e| e.get(id))
    {
        let added = ownership(entry)?;
        // An edited allowlist is not permission to restore a removed opt-in.
        ensure!((!added && allow.is_none()) || has_id, POLICY);
        return Ok(text.to_owned());
    }
    let allow_added = allow.is_some() && !has_id;
    let entry = owned(allow_added);
    let encoded_id = serde_json::to_string(id).map_err(|_| anyhow::anyhow!(INVALID))?;
    let encoded_entry = serde_json::to_string(&entry).map_err(|_| anyhow::anyhow!(INVALID))?;
    let body = format!("{encoded_id}:{encoded_entry}");
    let mut edits = Vec::new();
    if let Some(plugin_node) = doc.root.field("plugins") {
        if let Some(entries) = plugin_node.field("entries") {
            append(&doc, entries, body, &mut edits);
        } else {
            append(
                &doc,
                plugin_node,
                format!("\"entries\":{{{body}}}"),
                &mut edits,
            );
        }
        if allow_added {
            let list = plugin_node
                .field("allow")
                .ok_or_else(|| anyhow::anyhow!(INVALID))?;
            append(&doc, list, encoded_id, &mut edits);
        }
    } else {
        append(
            &doc,
            &doc.root,
            format!("\"plugins\":{{\"entries\":{{{body}}}}}"),
            &mut edits,
        );
    }
    let mut expected = doc.value.clone();
    let root = expected
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!(INVALID))?;
    let plugins = root.entry("plugins").or_insert_with(|| json!({}));
    let plugins = plugins
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!(INVALID))?;
    let entries = plugins.entry("entries").or_insert_with(|| json!({}));
    entries
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!(INVALID))?
        .insert(id.to_owned(), entry);
    if allow_added {
        plugins
            .get_mut("allow")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| anyhow::anyhow!(INVALID))?
            .push(Value::String(id.to_owned()));
    }
    finish(text, edits, expected)
}

pub(super) fn disable_owned(text: &str, id: &str) -> Result<String> {
    let doc = parse(text)?;
    policy(&doc, id)?;
    let Some(plugin_node) = doc.root.field("plugins") else {
        return Ok(text.to_owned());
    };
    let Some(entries) = plugin_node.field("entries") else {
        return Ok(text.to_owned());
    };
    let Some(member) = entries.member(id) else {
        return Ok(text.to_owned());
    };
    let allow_added = ownership(&doc.value["plugins"]["entries"][id])?;
    let mut edits = Vec::new();
    remove(&doc, member.start..member.value.tokens.end, &mut edits);
    let mut expected = doc.value.clone();
    expected["plugins"]["entries"]
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!(INVALID))?
        .remove(id);
    if allow_added {
        if let Some(items) = doc.value["plugins"].get("allow").and_then(Value::as_array) {
            if let Some(i) = items.iter().position(|v| v.as_str() == Some(id)) {
                let list = plugin_node
                    .field("allow")
                    .ok_or_else(|| anyhow::anyhow!(INVALID))?;
                let Shape::Array(nodes) = &list.shape else {
                    bail!(INVALID);
                };
                remove(&doc, nodes[i].tokens.clone(), &mut edits);
                expected["plugins"]["allow"]
                    .as_array_mut()
                    .ok_or_else(|| anyhow::anyhow!(INVALID))?
                    .remove(i);
            }
        }
    }
    // Explicit disabled/deny policy can remain in force during owned cleanup.
    finish(text, edits, expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "pleat-openclaw-adaptation";

    fn value(text: &str) -> Value {
        json5::from_str(text).unwrap()
    }

    #[test]
    fn append_keeps_separator_before_body_when_insert_offsets_coincide() {
        for (input, expected) in [
            ("{x:1}", "{x:1,\"p\":2}"),
            ("{x:{}}", "{x:{},\"p\":2}"),
            ("{x:[]}", "{x:[],\"p\":2}"),
            ("{x:1 /*keep*/}", "{x:1, /*keep*/\"p\":2}"),
            ("{x:1,}", "{x:1,\"p\":2,}"),
        ] {
            let doc = parse(input).unwrap();
            let mut edits = Vec::new();
            append(&doc, &doc.root, "\"p\":2".to_owned(), &mut edits);
            assert_eq!(finish(input, edits, value(expected)).unwrap(), expected);
        }
        for (input, expected) in [
            ("{items:['a']}", "{items:['a',\"p\"]}"),
            ("{items:['a' /*keep*/]}", "{items:['a', /*keep*/\"p\"]}"),
            ("{items:['a',]}", "{items:['a',\"p\",]}"),
        ] {
            let doc = parse(input).unwrap();
            let mut edits = Vec::new();
            append(
                &doc,
                doc.root.field("items").unwrap(),
                "\"p\"".to_owned(),
                &mut edits,
            );
            assert_eq!(finish(input, edits, value(expected)).unwrap(), expected);
        }
    }

    #[test]
    fn creates_only_missing_ancestors_and_is_idempotent() {
        for input in [
            "{}",
            "{ /*keep*/ }",
            "{other:1}",
            "{other:1,}",
            "{plugins:{}}",
            "{plugins:{entries:{}}}",
        ] {
            let enabled = enable(input, ID).unwrap();
            assert_eq!(value(&enabled)["plugins"]["entries"][ID], owned(false));
            assert!(value(&enabled)["plugins"].get("allow").is_none());
            assert_eq!(enable(&enabled, ID).unwrap(), enabled);
            let disabled = disable_owned(&enabled, ID).unwrap();
            assert_eq!(disable_owned(&disabled, ID).unwrap(), disabled);
            assert!(value(&disabled)["plugins"]["entries"].get(ID).is_none());
        }
    }

    #[test]
    fn json5_bytes_comments_crlf_quotes_and_trailing_commas_survive() {
        let input = "// head\r\n{ 'other':{n:0x12, f:+.5, text:'中\\\'文',},\r\n plugins:{allow:['a',/*allow*/],entries:{foreign:{enabled:false},/*entry*/},},\r\n}\r\n";
        let output = enable(input, ID).unwrap();
        let body = serde_json::to_string(&owned(true)).unwrap();
        let expected = input
            .replace("/*allow*/]", &format!("/*allow*/\"{ID}\",]"))
            .replace("/*entry*/}", &format!("/*entry*/\"{ID}\":{body},}}"));
        assert_eq!(output, expected);
        assert_eq!(disable_owned(&output, ID).unwrap(), input);
    }

    #[test]
    fn allow_ownership_is_never_taken_from_existing_policy() {
        for allow in ["['x']", "['x',]", "[]", "[/*keep*/]", "['x' //keep\n]"] {
            let input = format!("{{plugins:{{allow:{allow},entries:{{}}}}}}");
            assert_eq!(disable_owned(&input, ID).unwrap(), input);
            let output = enable(&input, ID).unwrap();
            assert_eq!(value(&output)["plugins"]["entries"][ID], owned(true));
            assert_eq!(enable(&output, ID).unwrap(), output);
            assert_eq!(disable_owned(&output, ID).unwrap(), input);
            assert_eq!(disable_owned(&input, ID).unwrap(), input);
        }
        let input = format!("{{plugins:{{allow:['{ID}','x'],entries:{{}}}}}}");
        assert_eq!(disable_owned(&input, ID).unwrap(), input);
        let output = enable(&input, ID).unwrap();
        assert_eq!(value(&output)["plugins"]["entries"][ID], owned(false));
        assert_eq!(disable_owned(&output, ID).unwrap(), input);
    }

    #[test]
    fn complete_owned_json5_with_escaped_keys_is_an_exact_noop() {
        let input = r#"{'plu\u0067ins':{allow:['p'],entries:{'\u0070':{
            enabled:true, hooks:{allowPromptInjection:true,allowConversationAccess:true,},
            config:{pleatManagedProfileVersion:1,pleatManagedAllowAdded:false,},
        },},}, other:{plugins:{allow:false}},}"#;
        assert_eq!(enable(input, "p").unwrap(), input);
        let removed = disable_owned(input, "p").unwrap();
        assert_eq!(value(&removed)["plugins"]["allow"], json!(["p"]));
        assert_eq!(value(&removed)["other"], value(input)["other"]);
        assert_eq!(disable_owned(&removed, "p").unwrap(), removed);
        let again = enable(&removed, "p").unwrap();
        assert_eq!(value(&again), value(input));
    }

    #[test]
    fn native_case_and_whitespace_aliases_cannot_hide_target_policy() {
        for input in [
            "{plugins:{deny:[' P ']}}",
            "{plugins:{allow:['P']}}",
            "{plugins:{entries:{P:{enabled:false}}}}",
            "{plugins:{entries:{' p ':{config:{thirdParty:1}}}}}",
            "{plugins:{entries:{p:{},P:{}}}}",
        ] {
            assert!(enable(input, "p").is_err());
            assert!(disable_owned(input, "p").is_err());
        }
        for id in ["", " ", " p", "p ", "__PROTO__", "constructor", "${ID}"] {
            assert!(enable("{}", id).is_err());
        }
    }

    #[test]
    fn explicit_disabled_deny_and_foreign_entries_are_conflicts() {
        for input in [
            "{plugins:{enabled:false}}".to_owned(),
            format!("{{plugins:{{deny:['{ID}']}}}}"),
            format!("{{plugins:{{entries:{{'{ID}':{{}}}}}}}}"),
            format!("{{plugins:{{entries:{{'{ID}':{{enabled:false}}}}}}}}"),
            format!("{{plugins:{{entries:{{'{ID}':{{config:{{thirdParty:'keep'}}}}}}}}}}"),
        ] {
            assert!(enable(&input, ID).is_err());
            if value(&input)["plugins"]["entries"].get(ID).is_some() {
                assert!(disable_owned(&input, ID).is_err());
            } else {
                assert_eq!(disable_owned(&input, ID).unwrap(), input);
            }
        }
        let input = "{plugins:{enabled:true,deny:['other'],entries:{other:{config:{keep:1}}}}}";
        let output = enable(input, ID).unwrap();
        assert_eq!(disable_owned(&output, ID).unwrap(), input);
    }

    #[test]
    fn every_partial_marker_or_additional_field_revokes_ownership() {
        let original = enable("{}", ID).unwrap();
        let mut entries = Vec::new();
        let mut entry = owned(false);
        entry["enabled"] = json!(false);
        entries.push(entry);
        let mut entry = owned(false);
        entry["hooks"]["allowPromptInjection"] = json!(false);
        entries.push(entry);
        let mut entry = owned(false);
        entry["hooks"]["allowConversationAccess"] = json!(false);
        entries.push(entry);
        let mut entry = owned(false);
        entry["hooks"]["timeoutMs"] = json!(123);
        entries.push(entry);
        let mut entry = owned(false);
        entry["config"]["pleatManagedProfileVersion"] = json!(2);
        entries.push(entry);
        let mut entry = owned(false);
        entry["config"]["pleatManagedAllowAdded"] = json!("false");
        entries.push(entry);
        let mut entry = owned(false);
        entry["config"]["thirdParty"] = json!(1);
        entries.push(entry);
        let mut entry = owned(false);
        entry.as_object_mut().unwrap().remove("enabled");
        entries.push(entry);
        let mut entry = owned(false);
        entry["config"]
            .as_object_mut()
            .unwrap()
            .remove("pleatManagedAllowAdded");
        entries.push(entry);
        let mut entry = owned(false);
        entry["subagent"] = json!({});
        entries.push(entry);
        for entry in entries {
            let mut changed = value(&original);
            changed["plugins"]["entries"][ID] = entry;
            let changed = changed.to_string();
            assert_eq!(enable(&changed, ID).unwrap_err().to_string(), OWNERSHIP);
            assert_eq!(
                disable_owned(&changed, ID).unwrap_err().to_string(),
                OWNERSHIP
            );
        }
    }

    #[test]
    fn removal_preserves_comments_even_inside_owned_entry() {
        let entry = serde_json::to_string(&owned(true))
            .unwrap()
            .replace("\"enabled\":true", "/*inner*/\"enabled\":true/*tail*/");
        for (before, after) in [("", ",other:{}"), ("other:{},", ","), ("other:{},", "")] {
            let input = format!(
                "{{plugins:{{entries:{{{before}/*key*/'{ID}'/*colon*/:{entry}{after}}},allow:['a',/*before*/'{ID}'/*after*/,'b',]}}}}"
            );
            let output = disable_owned(&input, ID).unwrap();
            for comment in [
                "/*key*/",
                "/*colon*/",
                "/*inner*/",
                "/*tail*/",
                "/*before*/",
                "/*after*/",
            ] {
                assert!(output.contains(comment));
            }
            assert_eq!(value(&output)["plugins"]["allow"], json!(["a", "b"]));
            assert_eq!(value(&output)["plugins"]["entries"], json!({"other":{}}));
        }
    }

    #[test]
    fn include_and_dynamic_policy_fail_closed_without_opening_anything() {
        for input in [
            "{$include:'sensitive.json5'}",
            "{plugins:{$include:['hidden.json5']}}",
            "{plugins:{entries:{$include:'hidden.json5'}}}",
            "{plugins:{allow:{$include:'hidden.json5'}}}",
            "{plugins:{deny:['${PLUGIN_ID}']}}",
            "{plugins:{allow:['$${PLUGIN_ID}']}}",
            "{plugins:'${POLICY}'}",
            "{plugins:{enabled:'${ENABLED}'}}",
            "{plugins:{entries:'${ENTRIES}'}}",
            "{plugins:{allow:'${ALLOW}'}}",
        ] {
            assert!(enable(input, ID).is_err());
            assert!(disable_owned(input, ID).is_err());
        }
        let input = "{unrelated:{$include:'do-not-read.json5',token:'${SECRET}'},plugins:{other:{$include:'opaque.json5'},entries:{}}}";
        let output = enable(input, ID).unwrap();
        assert_eq!(disable_owned(&output, ID).unwrap(), input);
    }

    #[test]
    fn rejects_invalid_types_duplicates_and_unsupported_syntax() {
        for input in [
            "",
            "null",
            "[]",
            "{plugins:null}",
            "{plugins:[]}",
            "{plugins:{entries:[]}}",
            "{plugins:{entries:{other:null}}}",
            "{plugins:{allow:[1]}}",
            "{plugins:{deny:null}}",
            "{plugins:{enabled:1}}",
            "{plugins:{},'plugins':{}}",
            "{plugins:{allow:[],allow:[]}}",
            "{plugins:{entries:{p:{},'p':{}}}}",
            "{plugins:{entries:{p:{hooks:{allowPromptInjection:true,allowPromptInjection:false}}}}}",
            r#"{plugins:{entries:{p:{config:{pleatManagedProfileVersion:1,"pleatManagedProfile\u0056ersion":1}}}}}"#,
            r#"{plugins:{},"plu\u0067ins":{}}"#,
            "{other:{nested:1,nested:2}}",
            "{x:NaN}",
            "{x:Infinity}",
            "{x:-Infinity}",
            "{x:1e999}",
            "{x:undefined}",
            "{x:()=>1}",
            "{x:[,]}",
            "{x:1,,}",
            "{}{}",
            "{x:'unterminated}",
            "{/*",
            "{]",
            "{x:1 y:2}",
            "{\\u0078:1}",
            "{键:1}",
            "{\u{a0}x:1}",
            "{//line\u{2028}x:1}",
        ] {
            assert!(enable(input, ID).is_err(), "unexpected success");
            assert!(disable_owned(input, ID).is_err(), "unexpected success");
        }
        let duplicate = format!("{{plugins:{{allow:['{ID}','{ID}']}}}}");
        assert!(enable(&duplicate, ID).is_err());
        assert!(disable_owned(&duplicate, ID).is_err());
    }

    #[test]
    fn bounded_before_deserialization_and_after_growth() {
        let exact = format!("{{}}{}", " ".repeat(MAX_BYTES - 2));
        assert_eq!(disable_owned(&exact, ID).unwrap(), exact);
        assert!(enable(&exact, ID).is_err());
        let oversized = format!("{exact} ");
        assert!(enable(&oversized, ID).is_err());
        assert!(disable_owned(&oversized, ID).is_err());
        let at_limit = format!(
            "{{x:{}0{}}}",
            "[".repeat(MAX_DEPTH - 1),
            "]".repeat(MAX_DEPTH - 1)
        );
        assert_eq!(disable_owned(&at_limit, ID).unwrap(), at_limit);
        let too_deep = format!("{{x:{}0{}}}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(enable(&too_deep, ID).is_err());
        assert!(disable_owned(&too_deep, ID).is_err());
    }

    #[test]
    fn escaped_id_is_data_and_errors_never_echo_config_or_id() {
        let id = "p'\\\"中";
        let enabled = enable("{plugins:{entries:{}}}", id).unwrap();
        assert_eq!(value(&enabled)["plugins"]["entries"][id], owned(false));
        assert_eq!(
            disable_owned(&enabled, id).unwrap(),
            "{plugins:{entries:{}}}"
        );
        let input = r#"{"private-config-secret":invalid}"#;
        for error in [
            enable(input, ID).unwrap_err(),
            disable_owned(input, ID).unwrap_err(),
            enable("{}", "private-id-secret\n").unwrap_err(),
        ] {
            let message = format!("{error:#}");
            assert!(!message.contains("private-"));
        }
    }

    #[test]
    fn pure_editor_does_not_claim_to_protect_stale_snapshots() {
        let preview = "{unrelated:1,plugins:{entries:{}}}";
        let concurrent = "{unrelated:2,plugins:{entries:{}}}";
        let candidate = enable(preview, ID).unwrap();
        assert_ne!(preview.as_bytes(), concurrent.as_bytes());
        assert_eq!(value(&candidate)["unrelated"], json!(1));
        // This function has no filesystem. ConfigPlan MUST reject this candidate
        // on snapshot mismatch; regeneration must use the new full snapshot.
        assert_eq!(
            value(&enable(concurrent, ID).unwrap())["unrelated"],
            json!(2)
        );
    }

    #[test]
    fn semantic_guard_rejects_an_unrelated_field_delta() {
        let input = "{x:1}";
        assert!(finish(input, vec![(3..4, "2".to_owned())], value(input)).is_err());
        assert_eq!(finish(input, Vec::new(), value(input)).unwrap(), input);
    }

    #[test]
    fn edited_allow_policy_is_not_reenabled_and_cleanup_does_not_broaden_it() {
        let enabled = enable("{plugins:{allow:[],entries:{}}}", ID).unwrap();
        let changed = enabled.replace(&format!("\"{ID}\"]"), "]");
        assert!(enable(&changed, ID).is_err());
        let disabled = disable_owned(&changed, ID).unwrap();
        assert_eq!(value(&disabled)["plugins"]["allow"], json!([]));
        let mut blocked = value(&enabled);
        blocked["plugins"]["enabled"] = json!(false);
        blocked["plugins"]["deny"] = json!([ID]);
        assert!(enable(&blocked.to_string(), ID).is_err());
        let disabled = value(&disable_owned(&blocked.to_string(), ID).unwrap());
        assert_eq!(disabled["plugins"]["enabled"], json!(false));
        assert_eq!(disabled["plugins"]["deny"], json!([ID]));
    }
}
