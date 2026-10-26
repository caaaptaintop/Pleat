//! Codex 0.163 公共手动入口的纯文本格式化层，尚未接入应用。
//!
//! 契约 pin：83086188ae0614bd882818ff1aa23f3194962012。
//! 依据：docs/agent-compatibility/native-guides/
//! codex-0-163-public-manual-link-current.json。
//!
//! 只验证可表达的文本语法；不访问文件系统、环境或原生 catalog。
//! 调用方必须先独立确认当前 Codex loaded/enabled 条目及其精确 source/path
//! 身份，再传入该条目的原始名称和原生路径。成功不证明选择命中或正文注入。
//! TUI 对失配路径可能回退到其它同名条目，不能只凭本函数成功就复制入口。
//! 不处理 ADP，也不转义、编码、规范化路径或追加 bare 名称回退。

use std::fmt;
use std::path::Path;

/// 格式化失败原因；不携带或回显输入路径、名称或环境内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualTextError {
    InvalidLabel,
    ReservedLabel,
    NonUtf8Path,
    RelativePath,
    UnsupportedWindowsPrefix,
    PathContainsControl,
    PathContainsRightParen,
    NotSkillMarkdown,
}

impl fmt::Display for ManualTextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidLabel => "技能名称须非空，且仅含 ASCII 字母、数字、下划线或连字符。",
            Self::ReservedLabel => "技能名称属于原生入口忽略的保留环境变量名，无法生成入口。",
            Self::NonUtf8Path => "原生技能路径不是有效 UTF-8，无法无损生成文本入口。",
            Self::RelativePath => "技能路径须为当前宿主的原生绝对路径，不接受相对路径或 URI。",
            Self::UnsupportedWindowsPrefix => "此 Windows 设备或逐字路径前缀没有已核验的公共文本契约。",
            Self::PathContainsControl => "技能路径含控制字符或换行分隔符，无法生成单行入口。",
            Self::PathContainsRightParen => "技能路径含右圆括号，原生文本语法无法无损表达。",
            Self::NotSkillMarkdown => "技能路径须直接以 SKILL.md 文件名结尾，保留其原始大小写。",
        })
    }
}

impl std::error::Error for ManualTextError {}

const RESERVED_ENV_LABELS: &[&str] = &[
    "PATH", "HOME", "USER", "SHELL", "PWD", "TMPDIR", "TEMP", "TMP", "LANG", "TERM",
    "XDG_CONFIG_HOME",
];

/// 生成单条 `[$name](absolute_native_skill_md)`，仅承诺文本格式。
///
/// 名称使用 Core、extension 和 TUI 的 ASCII 交集并排除保留名。
/// 路径须为宿主原生绝对路径；Windows 只接受普通盘符或 UNC 前缀。
/// 文件名按原生 selector 的 ASCII 大小写不敏感规则检查，输出不改大小写。
/// Unicode、内部空格、百分号、方括号、左圆括号和原生分隔符均原样保留。
/// 不读取或 canonicalize 文件；不能判断存在性、已加载、enabled 或 source 身份。
/// 调用方也不能用其它名称、猜测路径、file URI 或 bare 名称修复失败。
pub fn literal(name: &str, native_skill_md: &Path) -> Result<String, ManualTextError> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(ManualTextError::InvalidLabel);
    }
    if RESERVED_ENV_LABELS
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
    {
        return Err(ManualTextError::ReservedLabel);
    }

    let path = native_skill_md
        .to_str()
        .ok_or(ManualTextError::NonUtf8Path)?;
    if !native_skill_md.is_absolute() {
        return Err(ManualTextError::RelativePath);
    }
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        match native_skill_md.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(_) | Prefix::UNC(_, _) => {}
                _ => return Err(ManualTextError::UnsupportedWindowsPrefix),
            },
            _ => return Err(ManualTextError::UnsupportedWindowsPrefix),
        }
    }
    if path.chars().any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}') {
        return Err(ManualTextError::PathContainsControl);
    }
    if path.contains(')') {
        return Err(ManualTextError::PathContainsRightParen);
    }
    // 用原始文本取末段，避免 Path::file_name 忽略尾随分隔符或点而放宽契约。
    let filename = path
        .rsplit(|c| c == '/' || (cfg!(windows) && c == '\\'))
        .next()
        .unwrap_or("");
    if !filename.eq_ignore_ascii_case("SKILL.md") {
        return Err(ManualTextError::NotSkillMarkdown);
    }
    Ok(format!("[${name}]({path})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // 合成字符串，不读取真实 profile、环境或文件，也不模拟 loaded/enabled 判断。
    fn owned(tail: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:/OWNED/{tail}"))
        } else {
            PathBuf::from(format!("/OWNED/{tail}"))
        }
    }

    #[test]
    fn preserves_unicode_spaces_and_literal_punctuation() {
        let path = owned("来源 B/100%#?/[x]/SKILL.md");
        let expected = format!("[$demo]({})", path.to_str().unwrap());
        assert_eq!(literal("demo", &path), Ok(expected));
    }

    #[test]
    fn same_name_keeps_distinct_source_paths() {
        let a = owned("A/SKILL.md");
        let b = owned("B/SKILL.md");
        let a_text = literal("demo", &a).unwrap();
        let b_text = literal("demo", &b).unwrap();
        assert_eq!(a_text, format!("[$demo]({})", a.to_str().unwrap()));
        assert_eq!(b_text, format!("[$demo]({})", b.to_str().unwrap()));
        assert_ne!(a_text, b_text);
    }

    #[test]
    fn labels_accept_the_full_ascii_intersection() {
        for name in ["aZ09_-", "0demo", "_", "-", "home-skill"] {
            let path = owned("A/SKILL.md");
            assert_eq!(literal(name, &path), Ok(format!("[${name}]({})", path.to_str().unwrap())));
        }
    }

    #[test]
    fn rejects_backend_only_colon_and_other_invalid_labels() {
        for name in ["", "pkg:demo", "技能", "demo.name", "a b", "$demo", "[demo]", "a\nb"] {
            assert_eq!(literal(name, &owned("A/SKILL.md")), Err(ManualTextError::InvalidLabel));
        }
    }

    #[test]
    fn rejects_every_reserved_env_name_case_insensitively() {
        // 独立列出完整期望集合，避免遗漏常量时测试仍自我满足。
        for name in ["PATH", "HOME", "USER", "SHELL", "PWD", "TMPDIR", "TEMP", "TMP", "LANG", "TERM", "XDG_CONFIG_HOME"] {
            for spelling in [name.to_owned(), name.to_ascii_lowercase()] {
                assert_eq!(literal(&spelling, &owned("A/SKILL.md")), Err(ManualTextError::ReservedLabel));
            }
        }
        assert_eq!(literal("hOmE", &owned("A/SKILL.md")), Err(ManualTextError::ReservedLabel));
    }

    #[test]
    fn rejects_right_paren_including_markdown_backslash_escape() {
        for tail in ["B(v2)/SKILL.md", "B\\)/SKILL.md", "B)/other)/SKILL.md"] {
            assert_eq!(literal("demo", &owned(tail)), Err(ManualTextError::PathContainsRightParen));
        }
    }

    #[test]
    fn preserves_a_left_paren_without_balancing_or_escaping() {
        let path = owned("open(/SKILL.md");
        assert_eq!(literal("demo", &path), Ok(format!("[$demo]({})", path.to_str().unwrap())));
    }

    #[test]
    fn percent_text_is_not_encoded_or_decoded() {
        let space = owned("来源 B/SKILL.md");
        let percent = owned("来源%20B/%29/SKILL.md");
        assert_eq!(literal("demo", &space), Ok(format!("[$demo]({})", space.to_str().unwrap())));
        assert_eq!(literal("demo", &percent), Ok(format!("[$demo]({})", percent.to_str().unwrap())));
        assert_ne!(literal("demo", &space), literal("demo", &percent));
    }

    #[test]
    fn rejects_relative_tilde_and_uri_without_conversion() {
        for path in ["", "A/SKILL.md", "~/A/SKILL.md", "$HOME/A/SKILL.md", "file:///OWNED/A/SKILL.md", "skill:///OWNED/A/SKILL.md", "C:Skills/SKILL.md"] {
            assert_eq!(literal("demo", Path::new(path)), Err(ManualTextError::RelativePath));
        }
    }

    #[test]
    fn rejects_control_and_unicode_line_separators() {
        for c in ['\0', '\t', '\r', '\n', '\u{7f}', '\u{85}', '\u{2028}', '\u{2029}'] {
            assert_eq!(literal("demo", &owned(&format!("A{c}B/SKILL.md"))), Err(ManualTextError::PathContainsControl));
        }
    }

    #[test]
    fn requires_the_literal_final_filename_and_preserves_case() {
        for tail in ["A/ADAPTATION.md", "A/SKILL.md/", "A/SKILL.md/.", "A/SKILL.md ", "A/SKILL.md.txt"] {
            assert_eq!(literal("demo", &owned(tail)), Err(ManualTextError::NotSkillMarkdown));
        }
        let path = owned("A/sKiLl.Md");
        assert_eq!(literal("demo", &path), Ok(format!("[$demo]({})", path.to_str().unwrap())));
    }

    #[test]
    fn preserves_unicode_normalization_spelling() {
        let composed = owned("caf\u{e9}/SKILL.md");
        let decomposed = owned("cafe\u{301}/SKILL.md");
        assert_eq!(literal("demo", &composed), Ok(format!("[$demo]({})", composed.to_str().unwrap())));
        assert_eq!(literal("demo", &decomposed), Ok(format!("[$demo]({})", decomposed.to_str().unwrap())));
        assert_ne!(literal("demo", &composed), literal("demo", &decomposed));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_utf8_without_lossy_replacement() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(OsStr::from_bytes(b"/OWNED/\xff/SKILL.md"));
        assert_eq!(literal("demo", path), Err(ManualTextError::NonUtf8Path));
    }

    #[cfg(unix)]
    #[test]
    fn unix_keeps_backslash_as_filename_text() {
        let path = Path::new(r"/OWNED/A\B/SKILL.md");
        assert_eq!(literal("demo", path), Ok(r"[$demo](/OWNED/A\B/SKILL.md)".to_owned()));
        assert_eq!(literal("demo", Path::new(r"C:\Skills\Demo\SKILL.md")), Err(ManualTextError::RelativePath));
    }

    #[cfg(windows)]
    #[test]
    fn windows_preserves_exact_drive_unc_and_separator_spelling() {
        for path in [r"C:\Skills\Demo\SKILL.md", "C:/Skills/Demo/sKiLl.Md", r"\\server\share\Demo\SKILL.md"] {
            assert_eq!(literal("demo", Path::new(path)), Ok(format!("[$demo]({path})")));
        }
        for path in [r"\Skills\Demo\SKILL.md", r"C:Skills\Demo\SKILL.md"] {
            assert_eq!(literal("demo", Path::new(path)), Err(ManualTextError::RelativePath));
        }
        for path in [r"\\?\C:\Skills\Demo\SKILL.md", r"\\.\C:\Skills\Demo\SKILL.md"] {
            assert_eq!(literal("demo", Path::new(path)), Err(ManualTextError::UnsupportedWindowsPrefix));
        }
    }

    #[test]
    fn display_errors_are_fixed_chinese_messages_without_inputs() {
        for (error, expected) in [
            (ManualTextError::InvalidLabel, "技能名称须非空，且仅含 ASCII 字母、数字、下划线或连字符。"),
            (ManualTextError::ReservedLabel, "技能名称属于原生入口忽略的保留环境变量名，无法生成入口。"),
            (ManualTextError::NonUtf8Path, "原生技能路径不是有效 UTF-8，无法无损生成文本入口。"),
            (ManualTextError::RelativePath, "技能路径须为当前宿主的原生绝对路径，不接受相对路径或 URI。"),
            (ManualTextError::UnsupportedWindowsPrefix, "此 Windows 设备或逐字路径前缀没有已核验的公共文本契约。"),
            (ManualTextError::PathContainsControl, "技能路径含控制字符或换行分隔符，无法生成单行入口。"),
            (ManualTextError::PathContainsRightParen, "技能路径含右圆括号，原生文本语法无法无损表达。"),
            (ManualTextError::NotSkillMarkdown, "技能路径须直接以 SKILL.md 文件名结尾，保留其原始大小写。"),
        ] {
            assert_eq!(error.to_string(), expected);
        }
    }
}
