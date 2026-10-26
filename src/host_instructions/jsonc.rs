//! Byte-preserving edits for OpenCode's JSON-with-comments instruction list.
use anyhow::{Result, bail, ensure};
use std::ops::Range;

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
const INVALID: &str = "OpenCode JSONC 配置结构无效，已保留";

#[derive(Clone, Copy)]
struct Token {
    kind: u8,
    start: usize,
    end: usize,
}

// Bound nesting before invoking either a recursive parser or a deserializer.
// Only JSON tokens, JSON whitespace, comments and trailing commas are accepted.
fn lex(text: &str) -> Result<(Vec<Token>, Vec<Token>)> {
    ensure!(
        text.len() <= MAX_BYTES,
        "OpenCode JSONC 配置超过 2 MiB，已保留"
    );
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut comments = Vec::new();
    let mut stack = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let kind = bytes[i];
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
                comments.push(Token {
                    kind: b'/',
                    start,
                    end: i,
                });
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && &bytes[i..i + 2] != b"*/" {
                    i += 1;
                }
                ensure!(i + 1 < bytes.len(), INVALID);
                i += 2;
                comments.push(Token {
                    kind: b'*',
                    start,
                    end: i,
                });
                continue;
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
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
                    "OpenCode JSONC 配置嵌套超过 64 层，已保留"
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
            b',' | b':' => {
                i += 1;
            }
            _ => {
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
                    )
                {
                    i += 1;
                }
                ensure!(i > start, INVALID);
                tokens.push(Token {
                    kind: b'v',
                    start,
                    end: i,
                });
                continue;
            }
        }
        tokens.push(Token {
            kind,
            start,
            end: i,
        });
    }
    ensure!(stack.is_empty(), INVALID);
    Ok((tokens, comments))
}

struct Parser<'a> {
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

    fn value(&mut self) -> Result<Range<usize>> {
        let start = self.cursor;
        let kind = self
            .tokens
            .get(start)
            .ok_or_else(|| anyhow::anyhow!(INVALID))?
            .kind;
        self.cursor += 1;
        match kind {
            b'"' | b'v' => {}
            b'{' | b'[' => {
                let close = if kind == b'{' { b'}' } else { b']' };
                while !self.at(close) {
                    if kind == b'{' {
                        self.take(b'"')?;
                        self.take(b':')?;
                    }
                    self.value()?;
                    if !self.at(b',') {
                        break;
                    }
                    self.cursor += 1;
                }
                self.take(close)?;
            }
            _ => bail!(INVALID),
        }
        Ok(start..self.cursor)
    }
}

struct Document {
    tokens: Vec<Token>,
    comments: Vec<Token>,
    instructions: Option<Range<usize>>,
    last_value: Option<usize>,
    close: usize,
}

fn decode(text: &str, token: Token) -> Result<String> {
    serde_json::from_str(&text[token.start..token.end]).map_err(|_| anyhow::anyhow!(INVALID))
}

fn parse(text: &str) -> Result<Document> {
    let (tokens, comments) = lex(text)?;
    for &token in &tokens {
        if token.kind == b'"' {
            decode(text, token)?;
        } else if token.kind == b'v' {
            let scalar: serde_json::Value = serde_json::from_str(&text[token.start..token.end])
                .map_err(|_| anyhow::anyhow!(INVALID))?;
            ensure!(
                matches!(
                    scalar,
                    serde_json::Value::Null
                        | serde_json::Value::Bool(_)
                        | serde_json::Value::Number(_)
                ),
                INVALID
            );
        }
    }
    let mut parser = Parser {
        tokens: &tokens,
        cursor: 0,
    };
    parser.take(b'{')?;
    let mut instructions = None;
    let mut last_value = None;
    while !parser.at(b'}') {
        let key = parser.cursor;
        parser.take(b'"')?;
        parser.take(b':')?;
        let value = parser.value()?;
        last_value = Some(value.end - 1);
        if decode(text, tokens[key])? == "instructions" {
            ensure!(
                instructions.is_none(),
                "OpenCode instructions 字段重复，已保留"
            );
            instructions = Some(value);
        }
        if !parser.at(b',') {
            break;
        }
        parser.cursor += 1;
    }
    let close = parser.cursor;
    parser.take(b'}')?;
    ensure!(parser.cursor == tokens.len(), INVALID);
    // Validate through the existing serde parser, removing only JSONC trivia
    // in this temporary buffer. The original bytes remain the editing source.
    let mut json = String::with_capacity(text.len());
    for (i, token) in tokens.iter().enumerate() {
        if token.kind == b','
            && tokens
                .get(i + 1)
                .is_some_and(|t| matches!(t.kind, b']' | b'}'))
        {
            continue;
        }
        json.push_str(&text[token.start..token.end]);
    }
    let _: serde_json::Value = serde_json::from_str(&json).map_err(|_| anyhow::anyhow!(INVALID))?;
    if let Some(range) = &instructions {
        ensure!(
            tokens[range.start].kind == b'[',
            "OpenCode instructions 必须为字符串数组，已保留"
        );
        for token in &tokens[range.start + 1..range.end - 1] {
            ensure!(
                matches!(token.kind, b'"' | b','),
                "OpenCode instructions 必须为字符串数组，已保留"
            );
        }
    }
    Ok(Document {
        tokens,
        comments,
        instructions,
        last_value,
        close,
    })
}

fn matches(text: &str, doc: &Document, path: &str) -> Result<Vec<usize>> {
    let mut found = Vec::new();
    if let Some(range) = &doc.instructions {
        for i in range.start + 1..range.end - 1 {
            if doc.tokens[i].kind == b'"' && decode(text, doc.tokens[i])? == path {
                found.push(i);
            }
        }
    }
    Ok(found)
}

// Apply independent byte edits from right to left; all untouched bytes survive.
fn edit(text: &str, mut edits: Vec<(Range<usize>, String)>) -> String {
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut output = text.to_owned();
    for (range, replacement) in edits {
        output.replace_range(range, &replacement);
    }
    output
}

pub(super) fn add(text: &str, path: &str) -> Result<String> {
    let doc = parse(text)?;
    if !matches(text, &doc, path)?.is_empty() {
        return Ok(text.to_owned());
    }
    ensure!(
        path.len() <= MAX_BYTES,
        "OpenCode instructions 路径过大，已保留"
    );
    let encoded = serde_json::to_string(path).map_err(|_| anyhow::anyhow!(INVALID))?;
    insert(text, &doc, encoded)
}

fn insert(text: &str, doc: &Document, encoded: String) -> Result<String> {
    let (close, last, body) = if let Some(range) = &doc.instructions {
        let last = (range.end > range.start + 2)
            .then_some(range.end - 2)
            .filter(|&i| doc.tokens[i].kind != b',');
        (range.end - 1, last, encoded)
    } else {
        (
            doc.close,
            doc.last_value,
            format!("\"instructions\":[{encoded}]"),
        )
    };
    let at = doc.tokens[close].start;
    let mut edits = vec![(at..at, body)];
    if let Some(last) = last {
        if doc.tokens[last + 1].kind != b',' {
            let at = doc.tokens[last].end;
            edits.push((at..at, ",".to_owned()));
        }
    }
    let added_bytes: usize = edits.iter().map(|(_, replacement)| replacement.len()).sum();
    ensure!(
        text.len() + added_bytes <= MAX_BYTES,
        "OpenCode JSONC 编辑结果超过 2 MiB，已保留"
    );
    Ok(edit(text, edits))
}

pub(super) fn remove(text: &str, path: &str) -> Result<String> {
    let doc = parse(text)?;
    let found = matches(text, &doc, path)?;
    ensure!(
        found.len() <= 1,
        "OpenCode instructions 待移除路径重复，已保留"
    );
    let Some(&i) = found.first() else {
        return Ok(text.to_owned());
    };
    Ok(delete(text, &doc, i, Vec::new()))
}

fn delete(text: &str, doc: &Document, i: usize, mut edits: Vec<(Range<usize>, String)>) -> String {
    let token = doc.tokens[i];
    edits.push((token.start..token.end, String::new()));
    let comma = if doc.tokens[i + 1].kind == b',' {
        Some(i + 1)
    } else if doc.tokens[i - 1].kind == b',' {
        Some(i - 1)
    } else {
        None
    };
    if let Some(i) = comma {
        let token = doc.tokens[i];
        edits.push((token.start..token.end, String::new()));
    }
    edit(text, edits)
}

/// Whether a decoded path is already supplied, regardless of ownership.
pub(super) fn contains(text: &str, path: &str) -> Result<bool> {
    let doc = parse(text)?;
    Ok(!matches(text, &doc, path)?.is_empty())
}

fn validate_marker(marker: &str) -> Result<()> {
    ensure!(
        !marker.is_empty()
            && marker.len() <= 128
            && marker
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'-')),
        "OpenCode instructions 归属标记无效，已保留"
    );
    Ok(())
}

// The full emitted block comment must be the last non-whitespace text before
// this array string. A user comment interposed between them revokes ownership.
fn managed_entry(
    text: &str,
    doc: &Document,
    path: &str,
    marker: &str,
) -> Result<(Option<usize>, Option<Token>)> {
    let found = matches(text, doc, path)?;
    ensure!(
        found.len() <= 1,
        "OpenCode instructions 待管理路径重复，已保留"
    );
    let mut markers = doc.comments.iter().copied().filter(|comment| {
        let end = if comment.kind == b'*' {
            comment.end - 2
        } else {
            comment.end
        };
        text[comment.start + 2..end].trim() == marker
    });
    let comment = markers.next();
    ensure!(
        markers.next().is_none(),
        "OpenCode instructions 归属标记重复，已保留"
    );
    let index = found.first().copied();
    let owned = match (index, comment) {
        (Some(i), Some(comment))
            if comment.kind == b'*'
                && &text[comment.start..comment.end] == format!("/* {marker} */")
                && comment.end <= doc.tokens[i].start
                && text[comment.end..doc.tokens[i].start]
                    .bytes()
                    .all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n')) =>
        {
            Some(comment)
        }
        _ => None,
    };
    Ok((index, owned))
}

pub(super) fn is_managed(text: &str, path: &str, marker: &str) -> Result<bool> {
    validate_marker(marker)?;
    let doc = parse(text)?;
    Ok(managed_entry(text, &doc, path, marker)?.1.is_some())
}

pub(super) fn add_managed(text: &str, path: &str, marker: &str) -> Result<String> {
    validate_marker(marker)?;
    let doc = parse(text)?;
    let (existing, _) = managed_entry(text, &doc, path, marker)?;
    if existing.is_some() {
        return Ok(text.to_owned());
    }
    // Do not introduce a second copy of an orphaned or differently attached tag.
    for comment in &doc.comments {
        let end = if comment.kind == b'*' {
            comment.end - 2
        } else {
            comment.end
        };
        ensure!(
            text[comment.start + 2..end].trim() != marker,
            "OpenCode instructions 归属标记已占用，已保留"
        );
    }
    ensure!(
        path.len() <= MAX_BYTES,
        "OpenCode instructions 路径过大，已保留"
    );
    let encoded = serde_json::to_string(path).map_err(|_| anyhow::anyhow!(INVALID))?;
    insert(text, &doc, format!("/* {marker} */ {encoded}"))
}

pub(super) fn remove_managed(text: &str, path: &str, marker: &str) -> Result<String> {
    validate_marker(marker)?;
    let doc = parse(text)?;
    let (index, owned) = managed_entry(text, &doc, path, marker)?;
    match (index, owned) {
        (Some(i), Some(comment)) => Ok(delete(
            text,
            &doc,
            i,
            vec![(comment.start..comment.end, String::new())],
        )),
        _ => Ok(text.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_field_and_empty_arrays() {
        for (input, expected) in [
            ("{}", r#"{"instructions":["p"]}"#),
            ("{ /*keep*/ }", r#"{ /*keep*/ "instructions":["p"]}"#),
            (r#"{"x":1}"#, r#"{"x":1,"instructions":["p"]}"#),
            (
                r#"{"x":1, /*keep*/}"#,
                r#"{"x":1, /*keep*/"instructions":["p"]}"#,
            ),
            (r#"{"instructions":[]}"#, r#"{"instructions":["p"]}"#),
            (
                r#"{"instructions":[ /*keep*/ ]}"#,
                r#"{"instructions":[ /*keep*/ "p"]}"#,
            ),
        ] {
            assert_eq!(add(input, "p").unwrap(), expected);
            assert_eq!(remove(input, "absent").unwrap(), input);
            parse(expected).unwrap();
        }
    }

    #[test]
    fn append_preserves_trivia_and_trailing_commas() {
        for (input, expected) in [
            (r#"{"instructions":["a"]}"#, r#"{"instructions":["a","p"]}"#),
            (
                r#"{"instructions":["a", /*tail*/ ]}"#,
                r#"{"instructions":["a", /*tail*/ "p"]}"#,
            ),
            (
                "{\"instructions\":[\"a\" //tail\n]}",
                "{\"instructions\":[\"a\", //tail\n\"p\"]}",
            ),
        ] {
            assert_eq!(add(input, "p").unwrap(), expected);
            assert_eq!(add(expected, "p").unwrap(), expected);
            parse(expected).unwrap();
        }
    }

    #[test]
    fn deletion_at_each_position_only_removes_string_and_comma() {
        for (input, expected) in [
            (r#"["p", "b", "c"]"#, r#"[ "b", "c"]"#),
            (r#"["a", "p", "c"]"#, r#"["a",  "c"]"#),
            (r#"["a", "b", "p"]"#, r#"["a", "b" ]"#),
            (r#"["a", "p",]"#, r#"["a", ]"#),
            (r#"["p"]"#, "[]"),
            (
                r#"[/*before*/"p"/*after*/,/*tail*/]"#,
                "[/*before*//*after*//*tail*/]",
            ),
            (
                r#"["a"/*one*/,/*two*/"p"/*three*/]"#,
                r#"["a"/*one*//*two*//*three*/]"#,
            ),
        ] {
            let input = format!("{{\"instructions\":{input},\"keep\":true}}");
            let expected = format!("{{\"instructions\":{expected},\"keep\":true}}");
            assert_eq!(remove(&input, "p").unwrap(), expected);
            parse(&expected).unwrap();
        }
    }

    #[test]
    fn escapes_unicode_comments_and_nested_fields() {
        let input = r#"{ "instr\u0075ctions": ["\u4e2d\/a\\b\"c"], "x":{"instructions":false,"y":["} ] /*",{},],}, /* instructions: [ */ }"#;
        let path = "中/a\\b\"c";
        assert_eq!(add(input, path).unwrap(), input);
        let removed = remove(input, path).unwrap();
        assert_eq!(removed, input.replace(r#""\u4e2d\/a\\b\"c""#, ""));
        let escaped = "\"\\\n\t\r\0中文😀";
        let added = add(input, escaped).unwrap();
        assert_eq!(remove(&added, escaped).unwrap(), input);
        let line_comment = "{ // instructions: [\"p\"] }\n\"nested\": {\"instructions\": []}}";
        parse(&add(line_comment, "p").unwrap()).unwrap();
        let surrogate = r#"{"instructions":["\ud83d\ude00"]}"#;
        assert_eq!(add(surrogate, "😀").unwrap(), surrogate);
        assert_eq!(remove(surrogate, "😀").unwrap(), r#"{"instructions":[]}"#);
    }

    #[test]
    fn rejects_schema_duplicates_and_json5_extensions_without_echoing_input() {
        for input in [
            "",
            "[]",
            "null",
            r#"{"instructions":null}"#,
            r#"{"instructions":"p"}"#,
            r#"{"instructions":[1]}"#,
            r#"{"instructions":[{}]}"#,
            r#"{"instructions":[[]]}"#,
            r#"{"instructions":[],"instr\u0075ctions":[]}"#,
            "{instructions:[]}",
            "{'instructions':[]}",
            r#"{"x":NaN}"#,
            r#"{"x":Infinity}"#,
            r#"{"x":0x12}"#,
            r#"{"x":+1}"#,
            r#"{"x":.5}"#,
            r#"{"x":1.}"#,
            r#"{"x":"\x41"}"#,
            r#"{"x":"\v"}"#,
            r#"{"x":"\ud800"}"#,
            r#"{"x":"\q"}"#,
            "{\u{a0}}",
            "{\"x\":\"a\nb\"}",
            "{/*",
            "{ // no close",
            "{]",
            "{,}",
            r#"{"x":1,,}"#,
            r#"{"x":[,]}"#,
            r#"{"x":1 "y":2}"#,
            r#"{"instructions":["p" "b"]}"#,
            "{}{}",
        ] {
            assert!(add(input, "p").is_err(), "accepted {input:?}");
            assert!(remove(input, "p").is_err(), "accepted {input:?}");
        }
        let secret = r#"{"private-marker-123":invalid}"#;
        assert!(!format!("{:#}", add(secret, "p").unwrap_err()).contains("private-marker-123"));
        let duplicate = r#"{"instructions":["p","\u0070"]}"#;
        assert_eq!(add(duplicate, "p").unwrap(), duplicate);
        assert!(remove(duplicate, "p").is_err());
        assert_eq!(remove(duplicate, "absent").unwrap(), duplicate);
    }

    const MARKER: &str = "pleat-adaptation-v1:0123456789abcdef";

    #[test]
    fn managed_add_is_exact_idempotent_and_does_not_claim_external_paths() {
        let expected = format!(r#"{{"instructions":[/* {MARKER} */ "p"]}}"#);
        let added = add_managed("{}", "p", MARKER).unwrap();
        assert_eq!(added, expected);
        assert!(is_managed(&added, "p", MARKER).unwrap());
        assert!(contains(&added, "p").unwrap());
        assert_eq!(add_managed(&added, "p", MARKER).unwrap(), added);
        assert!(!is_managed(&added, "missing", MARKER).unwrap());
        assert!(!contains(&added, "missing").unwrap());
        assert!(!contains("{}", "p").unwrap());
        for input in [
            r#"{"instructions":["p"]}"#.to_owned(),
            r#"{"instructions":[/* user */ "\u0070"]}"#.to_owned(),
            format!(r#"{{"instructions":[/* {MARKER} */ /* user */ "p"]}}"#),
            format!(r#"{{"instructions":[/*{MARKER}*/ "p"]}}"#),
            format!(r#"{{"instructions":["p" /* {MARKER} */]}}"#),
            format!(r#"{{"instructions":[/* another */ "p"],"other":"/* {MARKER} */"}}"#),
        ] {
            assert!(!is_managed(&input, "p", MARKER).unwrap());
            assert_eq!(add_managed(&input, "p", MARKER).unwrap(), input);
            assert_eq!(remove_managed(&input, "p", MARKER).unwrap(), input);
        }
    }

    #[test]
    fn managed_remove_preserves_user_comments_in_all_positions() {
        for (array, expected) in [
            (
                format!(r#"[/* user */ /* {MARKER} */ "p", "b"]"#),
                r#"[/* user */   "b"]"#,
            ),
            (
                format!(r#"["a", /* {MARKER} */ "p" /*user*/, "b"]"#),
                r#"["a",   /*user*/ "b"]"#,
            ),
            (
                format!(r#"["a",/*user*/ /* {MARKER} */ "p"]"#),
                r#"["a"/*user*/  ]"#,
            ),
            (
                format!(r#"[/* {MARKER} */ "p", /*tail*/]"#),
                r#"[  /*tail*/]"#,
            ),
            (format!(r#"[/* {MARKER} */ "p"]"#), r#"[ ]"#),
        ] {
            let input = format!("{{\"instructions\":{array},\"x\":0}}");
            let expected = format!("{{\"instructions\":{expected},\"x\":0}}");
            assert_eq!(remove_managed(&input, "p", MARKER).unwrap(), expected);
            parse(&expected).unwrap();
        }
        let escaped = format!(r#"{{"instr\u0075ctions":[/* {MARKER} */ "\ud83d\ude00"]}}"#);
        assert!(is_managed(&escaped, "😀", MARKER).unwrap());
        assert_eq!(
            remove_managed(&escaped, "😀", MARKER).unwrap(),
            r#"{"instr\u0075ctions":[ ]}"#
        );
        let newline = format!("{{\"instructions\":[// user\n/* {MARKER} */ \"p\" // tail\n]}}");
        assert_eq!(
            remove_managed(&newline, "p", MARKER).unwrap(),
            "{\"instructions\":[// user\n  // tail\n]}"
        );
    }

    #[test]
    fn managed_add_preserves_existing_entries_trailing_commas_and_escapes() {
        for input in [
            r#"{"x":[1,],/*root*/}"#,
            r#"{"instructions":[/*empty*/]}"#,
            r#"{"instructions":["a",/*tail*/]}"#,
            "{\"instructions\":[\"a\" //tail\n]}",
        ] {
            let path = "中/\\\"\n😀";
            let added = add_managed(input, path, MARKER).unwrap();
            assert!(is_managed(&added, path, MARKER).unwrap());
            assert!(contains(&added, path).unwrap());
            assert_eq!(add_managed(&added, path, MARKER).unwrap(), added);
            let removed = remove_managed(&added, path, MARKER).unwrap();
            assert!(!contains(&removed, path).unwrap());
            assert!(!is_managed(&removed, path, MARKER).unwrap());
            parse(&removed).unwrap();
        }
    }

    #[test]
    fn managed_ambiguity_and_marker_injection_fail_closed() {
        for input in [
            r#"{"instructions":["p","\u0070"]}"#.to_owned(),
            format!(r#"{{"instructions":[/* {MARKER} */ "p",/* {MARKER} */ "q"]}}"#),
            format!(r#"{{/*{MARKER}*/ "instructions":[/* {MARKER} */ "p"]}}"#),
            format!("{{// {MARKER}\n\"instructions\":[/* {MARKER} */ \"p\"]}}"),
        ] {
            assert!(add_managed(&input, "p", MARKER).is_err());
            assert!(remove_managed(&input, "p", MARKER).is_err());
            assert!(is_managed(&input, "p", MARKER).is_err());
        }
        let orphan = format!(r#"{{/* {MARKER} */ "instructions":[]}}"#);
        assert!(add_managed(&orphan, "p", MARKER).is_err());
        assert!(!is_managed(&orphan, "p", MARKER).unwrap());
        assert_eq!(remove_managed(&orphan, "p", MARKER).unwrap(), orphan);
        for marker in ["", "*/", "x y", "x\ny", "x_y", "中文", &"a".repeat(129)] {
            assert!(add_managed("{}", "p", marker).is_err());
            assert!(remove_managed("{}", "p", marker).is_err());
            assert!(is_managed("{}", "p", marker).is_err());
        }
        assert!(add_managed("{}", "p", &"a".repeat(128)).is_ok());
        for input in [
            r#"{"instructions":false}"#,
            r#"{"instructions":[0]}"#,
            "{/*",
        ] {
            assert!(contains(input, "p").is_err());
            assert!(is_managed(input, "p", MARKER).is_err());
            assert!(add_managed(input, "p", MARKER).is_err());
            assert!(remove_managed(input, "p", MARKER).is_err());
        }
    }

    #[test]
    fn bounded_size_and_nesting_before_deserialization() {
        let at_limit = format!("{{}}{}", " ".repeat(MAX_BYTES - 2));
        assert_eq!(remove(&at_limit, "p").unwrap(), at_limit);
        assert!(add(&at_limit, "p").is_err());
        assert!(add(&(at_limit + " "), "p").is_err());
        let nested = |depth: usize| {
            format!(
                "{{\"x\":{}0{}}}",
                "[".repeat(depth - 1),
                "]".repeat(depth - 1)
            )
        };
        assert!(remove(&nested(MAX_DEPTH), "p").is_ok());
        assert!(remove(&nested(MAX_DEPTH + 1), "p").is_err());
        assert!(add(&nested(10_000), "p").is_err());
        assert!(remove("{\"x\":\"[[[[{{{{\",/* [[[[ */\"instructions\":[]}", "p").is_ok());
    }
}
