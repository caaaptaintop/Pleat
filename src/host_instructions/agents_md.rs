//! Byte preserving, owned native Markdown instruction entries.
//! Publication uses the existing native configuration CAS and recovery store.
use anyhow::{Result, ensure};
use std::{ops::Range, path::Path};

const INVALID: &str = "受管适配指令冲突或结构无效，已保留";
const MAX_BYTES: usize = 2 * 1024 * 1024;

fn block(path: &str, marker: &str, separator: bool) -> Result<String> {
    let adaptation = Path::new(path);
    let prefix = ["pleat-droid-adaptation-v1:", "pleat-copilot-adaptation-v1:"]
        .into_iter().find(|prefix| marker.starts_with(prefix)).ok_or_else(|| anyhow::anyhow!(INVALID))?;
    ensure!(adaptation.is_absolute() && path.len() <= 16 * 1024
        && !path.chars().any(char::is_control)
        && adaptation.file_name().is_some_and(|n| n == "ADAPTATION.md")
        && adaptation.parent().and_then(Path::file_name).is_some_and(|n| n == "local-data")
        && marker.len() == prefix.len() + 64
        && marker[prefix.len()..].bytes().all(|c| c.is_ascii_hexdigit()), INVALID);
    let installation = adaptation.parent().and_then(Path::parent).ok_or_else(|| anyhow::anyhow!(INVALID))?;
    let quoted_installation = serde_json::to_string(installation.to_str().ok_or_else(|| anyhow::anyhow!(INVALID))?)?;
    let quoted_adaptation = serde_json::to_string(path)?;
    Ok(format!(
        "{}<!-- {marker}:begin:{} -->\n\
        When this request actually uses the installed skill from {quoted_installation}, or its \
        resolved equivalent, read that skill's optional local adaptation at {quoted_adaptation} \
        with the native file reading tool before following the skill, and apply relevant instructions. \
        A same-named skill from another source does not match. Read each resolved adaptation at most \
        once per request. If the skill was not invoked or the optional file does not exist, continue \
        without reading it. Respect permission failures and native approval requirements. Do not \
        change trust, permissions or security settings, scan other skills or user directories, or run \
        commands to obtain it.\n<!-- {marker}:end -->\n",
        if separator { "\n" } else { "" }, u8::from(separator)))
}

fn owned_range(text: &str, path: &str, marker: &str) -> Result<Option<Range<usize>>> {
    ensure!(text.len() <= MAX_BYTES, INVALID);
    let mut found = None;
    for separator in [false, true] {
        let generated = block(path, marker, separator)?;
        for (at, _) in text.match_indices(&generated) {
            ensure!(found.is_none(), INVALID);
            found = Some(at..at + generated.len());
        }
    }
    ensure!(text.matches(marker).count() == if found.is_some() { 2 } else { 0 }, INVALID);
    Ok(found)
}

pub(super) fn contains(text: &str, path: &str, marker: &str) -> Result<bool> {
    Ok(owned_range(text, path, marker)?.is_some())
}

pub(super) fn enable(text: &str, path: &str, marker: &str) -> Result<String> {
    if owned_range(text, path, marker)?.is_some() { return Ok(text.to_owned()); }
    let addition = block(path, marker, !text.is_empty() && !text.ends_with('\n'))?;
    ensure!(text.len() + addition.len() <= MAX_BYTES, INVALID);
    Ok(format!("{text}{addition}"))
}

pub(super) fn disable_owned(text: &str, path: &str, marker: &str) -> Result<String> {
    let mut next = text.to_owned();
    if let Some(range) = owned_range(text, path, marker)? { next.replace_range(range, ""); }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    fn entry(name: &str) -> (String, String) {
        let path = std::path::absolute("project/.factory/skills").unwrap()
            .join(name).join("local-data/ADAPTATION.md").to_str().unwrap().to_owned();
        let marker = format!("pleat-droid-adaptation-v1:{:x}", Sha256::digest(path.as_bytes()));
        (path, marker)
    }
    #[test]
    fn round_trip_retains_user_bytes_and_joins_without_changing_them() {
        let (path, marker) = entry("含 空格");
        for before in ["", "# User instructions", "# 用户说明\r\n保留尾部  \r\n", "no final newline\r"] {
            let installed = enable(before, &path, &marker).unwrap();
            assert!(installed.starts_with(before));
            assert!(contains(&installed, &path, &marker).unwrap());
            assert_eq!(enable(&installed, &path, &marker).unwrap(), installed);
            assert_eq!(disable_owned(&installed, &path, &marker).unwrap(), before);
        }
    }
    #[test]
    fn independent_sources_and_user_edits_outside_owned_block_survive_removal() {
        let (a, ma) = entry("one/demo");
        let (b, mb) = entry("two/demo");
        let first = enable("user header\n", &a, &ma).unwrap();
        let both = enable(&first, &b, &mb).unwrap() + "\nuser new tail\n";
        let one = disable_owned(&both, &a, &ma).unwrap();
        assert!(contains(&one, &b, &mb).unwrap());
        assert_eq!(disable_owned(&one, &b, &mb).unwrap(), "user header\n\nuser new tail\n");
    }
    #[test]
    fn edited_duplicated_or_partial_owned_entries_are_never_adopted() {
        let (path, marker) = entry("demo");
        let valid = enable("", &path, &marker).unwrap();
        for text in [valid.replace("native file reading tool", "user changed instruction"),
                     format!("{valid}{valid}"), format!("<!-- {marker}:begin:0 -->\npartial") ] {
            assert!(enable(&text, &path, &marker).is_err());
            assert!(disable_owned(&text, &path, &marker).is_err());
        }
    }
    #[test]
    fn locator_cannot_inject_instructions_and_existing_large_files_are_preserved() {
        let (path, marker) = entry("demo");
        for bad in ["relative/local-data/ADAPTATION.md", "/x\n/new-rule/local-data/ADAPTATION.md", "/x/OTHER.md"] {
            assert!(enable("user", bad, &marker).is_err());
        }
        assert!(enable(&"x".repeat(MAX_BYTES), &path, &marker).is_err());
        assert_eq!(disable_owned("unmanaged user instructions", &path, &marker).unwrap(), "unmanaged user instructions");
    }
}
