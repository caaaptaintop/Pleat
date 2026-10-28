//! Codex rust-v0.163.0 的已知 catalog 位置观察；作者：admin。
//! 纯解析，不加载 native 配置栈，也不选择当前实例的 effective sqlite_home。
//! 不访问文件、环境、cwd 或数据库。所有路径均是待 reader 验证的候选。
use std::path::{Component, Path, PathBuf};

pub(super) const MAX_CONFIG_BYTES: usize = 1024 * 1024;
pub(super) const MAX_CONFIG_FILES: usize = 32;
pub(super) const MAX_TOTAL_CONFIG_BYTES: usize = 4 * MAX_CONFIG_BYTES;
pub(super) const MAX_PATH_BYTES: usize = 16 * 1024;
// 保守的语法预算：连字符串/注释内的标记也计费，在 TOML 解析前执行。
// 限制字段/数组/表膨胀；不是 TOML lexer，不以它判断字段是否存在。
pub(super) const MAX_CONFIG_MARKERS: usize = 4096;
pub(super) const MAX_CONFIG_DEPTH: usize = 64;

/// 调用者只能传固定原因，不能把 I/O 原文、TOML 或秘密放进诊断。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UnknownCode {
    NotObserved,
    Unreadable,
    InvalidEncoding,
    ConfigFileLimit,
    ConfigByteLimit,
    ConfigStructureLimit,
    InvalidToml,
    InvalidSqliteHomeType,
    PathLimit,
    InvalidPath,
    ConfigFileNotAbsolute,
    ResolvedHomeUnavailable,
    OsHomeUnavailable,
    NativeCwdUnavailable,
    UnobservedNativeLayers,
}

impl UnknownCode {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::NotObserved => "CCL_NOT_OBSERVED",
            Self::Unreadable => "CCL_UNREADABLE",
            Self::InvalidEncoding => "CCL_INVALID_ENCODING",
            Self::ConfigFileLimit => "CCL_CONFIG_FILE_LIMIT",
            Self::ConfigByteLimit => "CCL_CONFIG_BYTE_LIMIT",
            Self::ConfigStructureLimit => "CCL_CONFIG_STRUCTURE_LIMIT",
            Self::InvalidToml => "CCL_INVALID_TOML",
            Self::InvalidSqliteHomeType => "CCL_INVALID_SQLITE_HOME_TYPE",
            Self::PathLimit => "CCL_PATH_LIMIT",
            Self::InvalidPath => "CCL_INVALID_PATH",
            Self::ConfigFileNotAbsolute => "CCL_CONFIG_FILE_NOT_ABSOLUTE",
            Self::ResolvedHomeUnavailable => "CCL_RESOLVED_HOME_UNAVAILABLE",
            Self::OsHomeUnavailable => "CCL_OS_HOME_UNAVAILABLE",
            Self::NativeCwdUnavailable => "CCL_NATIVE_CWD_UNAVAILABLE",
            Self::UnobservedNativeLayers => "CCL_UNOBSERVED_NATIVE_LAYERS",
        }
    }
}

// 故意不实现 Debug：输入中的整份 TOML/env 不应被顺手日志输出。
#[derive(Clone, Copy)]
pub(super) enum Observed<T> {
    KnownAbsent,
    Known(T),
    Unknown(UnknownCode),
}

#[derive(Clone, Copy)]
pub(super) enum SqliteHomeEnv<'a> {
    KnownAbsent,
    Known(&'a str),
    /// 对应官方 env::var(...).ok()：非 Unicode 是已知无 env override。
    NonUnicode,
    Unknown(UnknownCode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConfigKind { System, User }

pub(super) struct ConfigInput<'a> {
    pub(super) kind: ConfigKind,
    /// 调用者确认来源的绝对配置文件路径；相对 sqlite_home 用其 parent。
    /// 不能把未选中的 profile、Project 或 managed 文件冒充 User/System。
    pub(super) file: &'a Path,
    pub(super) text: Observed<&'a str>,
}

pub(super) struct KnownCatalogInputs<'a> {
    pub(super) os_home: Observed<&'a Path>,
    /// 已由调用者解析的 native Codex home；本模块不解析 CODEX_HOME。
    pub(super) resolved_codex_home: Observed<&'a Path>,
    /// 必须是该 native host 的 resolved_cwd，不能使用 Pleat cwd 代替。
    pub(super) native_resolved_cwd: Option<&'a Path>,
    pub(super) configs: &'a [ConfigInput<'a>],
    pub(super) sqlite_home_env: SqliteHomeEnv<'a>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source {
    DefaultCodexHome,
    Config { index: usize, kind: ConfigKind },
    ConfigBatch,
    Environment,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Origin {
    pub(super) source: Source,
    /// 已规范化的配置来源元数据；诊断不携带该路径。
    pub(super) config_file: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CatalogCandidate {
    /// SQLite 数据目录，不是项目目录；reader 固定追加 state_5.sqlite。
    pub(super) root: PathBuf,
    pub(super) origins: Vec<Origin>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ObservationStatus {
    KnownAbsent,
    Candidate { index: usize },
    Unknown(UnknownCode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SourceObservation {
    pub(super) source: Source,
    pub(super) status: ObservationStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Diagnostic {
    pub(super) source: Source,
    pub(super) code: UnknownCode,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct KnownCatalogLocations {
    pub(super) candidates: Vec<CatalogCandidate>,
    pub(super) observations: Vec<SourceObservation>,
    pub(super) diagnostics: Vec<Diagnostic>,
    /// 即使所有传入的文件都已知，也没有完整 native 层/启动上下文证据。
    /// 该字段不参与候选过滤，不能据它清空已确认的 catalog 登记。
    pub(super) active_context: ObservationStatus,
}

/// 顺序是检查顺序，不是 config/env 优先级选举。
/// 始终独立保留默认目录候选；每个已知 config/env 追加来源明确的候选。
/// 相同词法 root 只读一次；实体别名仍交既有 reader/父层身份机制处理。
pub(super) fn known_catalog_locations(input: &KnownCatalogInputs<'_>) -> KnownCatalogLocations {
    let mut out = KnownCatalogLocations {
        candidates: Vec::new(), observations: Vec::new(), diagnostics: Vec::new(),
        active_context: ObservationStatus::Unknown(UnknownCode::UnobservedNativeLayers),
    };
    let default = match input.resolved_codex_home {
        Observed::Known(path) => absolute_metadata_path(path, UnknownCode::InvalidPath).map(Some),
        Observed::KnownAbsent => Err(UnknownCode::ResolvedHomeUnavailable),
        Observed::Unknown(code) => Err(code),
    };
    out.record(Source::DefaultCodexHome, None, default);

    // 数量先判，不遍历/分配超限来源。默认/env 的独立观察仍继续。
    if input.configs.len() > MAX_CONFIG_FILES {
        out.record(Source::ConfigBatch, None, Err(UnknownCode::ConfigFileLimit));
    } else {
        let mut bytes = 0usize;
        for (index, config) in input.configs.iter().enumerate() {
            let source = Source::Config { index, kind: config.kind };
            // 每份输入在解析、路径克隆前计费；拒绝的超大文件不消耗后续预算。
            if let Observed::Known(text) = config.text {
                if text.len() > MAX_CONFIG_BYTES
                    || text.len() > MAX_TOTAL_CONFIG_BYTES.saturating_sub(bytes) {
                    out.record(source, None, Err(UnknownCode::ConfigByteLimit));
                    continue;
                }
                bytes += text.len();
            }
            let file = match absolute_metadata_path(config.file, UnknownCode::ConfigFileNotAbsolute) {
                Ok(file) if file.file_name().is_some() => file,
                Ok(_) => {
                    out.record(source, None, Err(UnknownCode::ConfigFileNotAbsolute));
                    continue;
                }
                Err(code) => {
                    out.record(source, None, Err(code));
                    continue;
                }
            };
            let result = match config.text {
                Observed::KnownAbsent => Ok(None),
                Observed::Unknown(code) => Err(code),
                Observed::Known(text) => config_root(text, file.parent().unwrap(), input.os_home),
            };
            out.record(source, Some(file), result);
        }
    }

    let env = match input.sqlite_home_env {
        SqliteHomeEnv::KnownAbsent | SqliteHomeEnv::NonUnicode => Ok(None),
        SqliteHomeEnv::Unknown(code) => Err(code),
        SqliteHomeEnv::Known(raw) => {
            // 必须在 trim 扫描和任何分配之前拒绝超长原始 env。
            if raw.len() > MAX_PATH_BYTES { Err(UnknownCode::PathLimit) }
            else if raw.trim().is_empty() { Ok(None) }
            else {
                resolve_path(raw.trim(), input.native_resolved_cwd, input.os_home,
                    UnknownCode::NativeCwdUnavailable).map(Some)
            }
        }
    };
    out.record(Source::Environment, None, env);
    out
}

impl KnownCatalogLocations {
    fn record(&mut self, source: Source, file: Option<PathBuf>, result: Result<Option<PathBuf>, UnknownCode>) {
        let status = match result {
            Ok(None) => ObservationStatus::KnownAbsent,
            Err(code) => {
                self.diagnostics.push(Diagnostic { source, code });
                ObservationStatus::Unknown(code)
            }
            Ok(Some(root)) => {
                let origin = Origin { source, config_file: file };
                let index = if let Some(index) = self.candidates.iter().position(|item| item.root == root) {
                    // source 包含输入 index，同 root 的不同观察都保留。
                    self.candidates[index].origins.push(origin);
                    index
                } else {
                    self.candidates.push(CatalogCandidate { root, origins: vec![origin] });
                    self.candidates.len() - 1
                };
                ObservationStatus::Candidate { index }
            }
        };
        self.observations.push(SourceObservation { source, status });
    }
}

fn config_root(text: &str, base: &Path, home: Observed<&Path>) -> Result<Option<PathBuf>, UnknownCode> {
    if text.len() > MAX_CONFIG_BYTES { return Err(UnknownCode::ConfigByteLimit); }
    let mut markers = 0usize;
    let mut depth = 0usize;
    let mut line_dots = 0usize;
    for byte in text.bytes() {
        if matches!(byte, b'=' | b'[' | b']' | b'{' | b'}' | b',' | b'.') {
            markers += 1;
            if markers > MAX_CONFIG_MARKERS { return Err(UnknownCode::ConfigStructureLimit); }
        }
        // 保守地限制嵌套和 dotted-key 链；字符串/注释同样计费。
        // 不依赖 TOML 库版本的递归上限来抵御深层输入。
        match byte {
            b'[' | b'{' => depth += 1,
            b']' | b'}' => depth = depth.saturating_sub(1),
            b'.' => line_dots += 1,
            b'\n' => line_dots = 0,
            _ => {},
        }
        if depth > MAX_CONFIG_DEPTH || line_dots > MAX_CONFIG_DEPTH {
            return Err(UnknownCode::ConfigStructureLimit);
        }
    }
    // 错误对象在此丢弃；从不向输出保留 TOML、raw error 或其他字段。
    let value = toml::from_str::<toml::Value>(text).map_err(|_| UnknownCode::InvalidToml)?;
    match value.get("sqlite_home") {
        None => Ok(None),
        Some(toml::Value::String(raw)) => resolve_path(raw, Some(base), home,
            UnknownCode::ConfigFileNotAbsolute).map(Some),
        Some(_) => Err(UnknownCode::InvalidSqliteHomeType),
    }
}

fn checked_text(path: &Path) -> Result<&str, UnknownCode> {
    if path.as_os_str().len() > MAX_PATH_BYTES { return Err(UnknownCode::PathLimit); }
    let text = path.to_str().ok_or(UnknownCode::InvalidEncoding)?;
    if text.contains('\0') { return Err(UnknownCode::InvalidPath); }
    Ok(text)
}

fn absolute_metadata_path(path: &Path, invalid: UnknownCode) -> Result<PathBuf, UnknownCode> {
    checked_text(path)?;
    if !path.is_absolute() { return Err(invalid); }
    Ok(lexical(&platform_path(path)))
}

fn resolve_path(raw: &str, base: Option<&Path>, home: Observed<&Path>, missing_base: UnknownCode)
    -> Result<PathBuf, UnknownCode>
{
    if raw.len() > MAX_PATH_BYTES { return Err(UnknownCode::PathLimit); }
    if raw.contains('\0') { return Err(UnknownCode::InvalidPath); }
    let expanded;
    let tilde_tail = if raw == "~" { Some("") }
        else if let Some(rest) = raw.strip_prefix("~/") { Some(rest.trim_start_matches('/')) }
        else if cfg!(windows) {
            raw.strip_prefix("~\\").map(|rest| rest.trim_start_matches('\\'))
        } else { None };
    let raw_path = if let Some(tail) = tilde_tail {
        let home = match home {
            Observed::Known(path) => absolute_metadata_path(path, UnknownCode::OsHomeUnavailable)?,
            Observed::KnownAbsent => return Err(UnknownCode::OsHomeUnavailable),
            Observed::Unknown(code) => return Err(code),
        };
        // tail 的前导 / 已移除：~// 不得覆盖 home 成为根目录。
        check_join_budget(&home, tail)?;
        expanded = home.join(tail);
        expanded.as_path()
    } else { Path::new(raw) };
    let platform_path = platform_path(raw_path);
    let path = if platform_path.is_absolute() { platform_path }
    else {
        let base = base.ok_or(missing_base)?;
        let base = absolute_metadata_path(base, missing_base)?;
        check_join_budget(&base, checked_text(&platform_path)?)?;
        with_base(&platform_path, &base)
    };
    let result = lexical(&path);
    checked_text(&result)?;
    if !result.is_absolute() { return Err(UnknownCode::InvalidPath); }
    Ok(result)
}

fn check_join_budget(base: &Path, tail: &str) -> Result<(), UnknownCode> {
    // 保守限制连接前长度；不靠先构造巨大路径再用 .. 缩小来通过预算。
    if base.as_os_str().len().saturating_add(1).saturating_add(tail.len()) > MAX_PATH_BYTES {
        return Err(UnknownCode::PathLimit);
    }
    Ok(())
}

fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {},
            Component::ParentDir => { out.pop(); },
            _ => out.push(part.as_os_str()),
        }
    }
    out
}

#[cfg(not(windows))]
fn platform_path(path: &Path) -> PathBuf { path.to_path_buf() }

#[cfg(windows)]
fn platform_path(path: &Path) -> PathBuf {
    let text = path.to_str().unwrap(); // 已经过 checked_text/raw 校验。
    for prefix in [r"\\?\UNC\", r"\\.\UNC\"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return PathBuf::from(format!(r"\\{rest}"));
        }
    }
    for prefix in [r"\\?\", r"\\.\"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            let bytes = rest.as_bytes();
            if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
                && matches!(bytes[2], b'\\' | b'/') { return PathBuf::from(rest); }
        }
    }
    path.to_path_buf()
}

#[cfg(not(windows))]
fn with_base(path: &Path, base: &Path) -> PathBuf { base.join(path) }

#[cfg(windows)]
fn with_base(path: &Path, base: &Path) -> PathBuf {
    if path.has_root() { return base.join(path); }
    let mut parts = path.components();
    let Some(Component::Prefix(prefix)) = parts.next() else { return base.join(path); };
    let mut out = PathBuf::from(prefix.as_os_str());
    if parts.clone().next().is_none() { out.push(std::path::MAIN_SEPARATOR_STR); }
    else {
        // 官方 drive-relative 规则：沿用 base 尾部，替换其 drive prefix。
        out.extend(base.components().skip(1).chain(parts));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // 全部为内存合成语义 fixture；无文件、DB、env 或 native 登记证明。
    fn p(tail: &str) -> PathBuf {
        if cfg!(windows) { PathBuf::from(r"C:\").join(tail) }
        else { PathBuf::from("/").join(tail) }
    }

    fn inputs<'a>(home: &'a Path, configs: &'a [ConfigInput<'a>], env: SqliteHomeEnv<'a>) -> KnownCatalogInputs<'a> {
        KnownCatalogInputs { os_home: Observed::Known(home), resolved_codex_home: Observed::Known(home),
            native_resolved_cwd: None, configs, sqlite_home_env: env }
    }

    fn status(out: &KnownCatalogLocations, source: Source) -> ObservationStatus {
        out.observations.iter().find(|item| item.source == source).unwrap().status
    }

    #[test]
    fn config_and_env_are_separate_candidates_not_an_effective_root_election() {
        let home = p("native-home");
        let file = p("user/config.toml");
        let env = p("env-db");
        let configs = [ConfigInput { kind: ConfigKind::User, file: &file,
            text: Observed::Known("sqlite_home = 'config-db'") }];
        let out = known_catalog_locations(&inputs(&home, &configs, SqliteHomeEnv::Known(env.to_str().unwrap())));
        assert_eq!(out.candidates.iter().map(|c| c.root.clone()).collect::<Vec<_>>(),
            [home, p("user/config-db"), env]);
        assert_eq!(out.candidates[1].origins[0].source, Source::Config { index: 0, kind: ConfigKind::User });
        assert_eq!(out.candidates[2].origins[0].source, Source::Environment);
        assert_eq!(out.active_context, ObservationStatus::Unknown(UnknownCode::UnobservedNativeLayers));
    }

    #[test]
    fn every_config_uses_its_own_file_parent_and_preserves_order() {
        let home = p("native-home");
        let system = p("etc/codex/config.toml");
        let user = p("custom/user/config.toml");
        let configs = [
            ConfigInput { kind: ConfigKind::System, file: &system, text: Observed::Known("sqlite_home = '../db'") },
            ConfigInput { kind: ConfigKind::User, file: &user, text: Observed::Known("sqlite_home = './db'") },
        ];
        let out = known_catalog_locations(&inputs(&home, &configs, SqliteHomeEnv::KnownAbsent));
        assert_eq!(out.candidates[1].root, p("etc/db"));
        assert_eq!(out.candidates[2].root, p("custom/user/db"));
        assert_eq!(out.candidates[1].origins[0].config_file, Some(system));
    }

    #[test]
    fn tilde_double_slash_missing_parent_and_root_parent_are_lexical() {
        let home = p("os-home");
        for (raw, expected) in [("~//missing/../db", p("os-home/db")),
            ("~", home.clone()), ("missing/../db", p("base/db")),
            ("alias/../db", p("base/db")), ("~someone/db", p("base/~someone/db")),
            ("", p("base")), (" ", p("base/ "))] {
            assert_eq!(resolve_path(raw, Some(&p("base")), Observed::Known(&home),
                UnknownCode::NativeCwdUnavailable).unwrap(), expected);
        }
        let raw = p("../../db");
        assert_eq!(resolve_path(raw.to_str().unwrap(), None, Observed::KnownAbsent,
            UnknownCode::NativeCwdUnavailable).unwrap(), p("db"));
    }

    #[test]
    fn relative_env_needs_native_cwd_and_never_uses_codex_home() {
        let home = p("codex-home");
        let mut input = inputs(&home, &[], SqliteHomeEnv::Known("relative/../db"));
        let out = known_catalog_locations(&input);
        assert_eq!(out.candidates.len(), 1);
        assert_eq!(status(&out, Source::Environment), ObservationStatus::Unknown(UnknownCode::NativeCwdUnavailable));
        let cwd = p("actual-native-cwd");
        input.native_resolved_cwd = Some(&cwd);
        assert_eq!(known_catalog_locations(&input).candidates[1].root, cwd.join("db"));
    }

    #[test]
    fn absent_empty_whitespace_non_unicode_are_known_absent_not_unknown() {
        let home = p("native-home");
        for env in [SqliteHomeEnv::KnownAbsent, SqliteHomeEnv::Known(""), SqliteHomeEnv::Known(" \n\t\u{2003}"), SqliteHomeEnv::NonUnicode] {
            let out = known_catalog_locations(&inputs(&home, &[], env));
            assert_eq!(status(&out, Source::Environment), ObservationStatus::KnownAbsent);
            assert_eq!(out.candidates.len(), 1);
            assert!(out.diagnostics.is_empty());
        }
        let raw = format!(" \t{} \n", p("env-db").display());
        assert_eq!(known_catalog_locations(&inputs(&home, &[], SqliteHomeEnv::Known(&raw))).candidates[1].root, p("env-db"));
    }

    #[test]
    fn unknown_sources_never_erase_known_candidates_or_become_absent() {
        let home = p("native-home");
        let file = p("user/config.toml");
        let configs = [
            ConfigInput { kind: ConfigKind::User, file: &file, text: Observed::Unknown(UnknownCode::Unreadable) },
            ConfigInput { kind: ConfigKind::System, file: &file, text: Observed::Known("sqlite_home = 'observed-db'") },
        ];
        let out = known_catalog_locations(&inputs(&home, &configs, SqliteHomeEnv::Unknown(UnknownCode::NotObserved)));
        assert_eq!(out.candidates.len(), 2);
        assert_eq!(out.candidates[1].root, p("user/observed-db"));
        assert_eq!(out.diagnostics.len(), 2);
        assert_eq!(status(&out, Source::Config { index: 0, kind: ConfigKind::User }), ObservationStatus::Unknown(UnknownCode::Unreadable));
        assert_eq!(status(&out, Source::Environment), ObservationStatus::Unknown(UnknownCode::NotObserved));
    }

    #[test]
    fn same_root_merges_reads_and_keeps_all_origins() {
        let home = p("same");
        let system = home.join("system.toml");
        let user = home.join("user.toml");
        let configs = [
            ConfigInput { kind: ConfigKind::System, file: &system, text: Observed::Known("sqlite_home = '.'") },
            ConfigInput { kind: ConfigKind::User, file: &user, text: Observed::Known("sqlite_home = 'missing/..'") },
        ];
        let out = known_catalog_locations(&inputs(&home, &configs, SqliteHomeEnv::Known(home.to_str().unwrap())));
        assert_eq!(out.candidates.len(), 1);
        assert_eq!(out.candidates[0].origins.len(), 4);
        assert_eq!(out.observations.len(), 4);
        assert!(out.observations.iter().all(|o| o.status == ObservationStatus::Candidate { index: 0 }));
    }

    #[test]
    fn field_absence_is_local_and_unselected_profile_is_not_a_root() {
        let home = p("native-home");
        let file = p("user/config.toml");
        for text in [Observed::KnownAbsent, Observed::Known(""),
            Observed::Known("[profiles.other]\nsqlite_home = '/not-selected'")] {
            let configs = [ConfigInput { kind: ConfigKind::User, file: &file, text }];
            let out = known_catalog_locations(&inputs(&home, &configs, SqliteHomeEnv::KnownAbsent));
            assert_eq!(out.candidates.len(), 1);
            assert_eq!(status(&out, Source::Config { index: 0, kind: ConfigKind::User }), ObservationStatus::KnownAbsent);
            assert!(matches!(out.active_context, ObservationStatus::Unknown(_)));
        }
    }

    #[test]
    fn malformed_toml_types_and_nul_do_not_escape_through_diagnostics() {
        let home = p("native-home");
        let file = p("user/config.toml");
        for (text, code) in [("sqlite_home = [", UnknownCode::InvalidToml),
            ("sqlite_home = 7", UnknownCode::InvalidSqliteHomeType),
            ("sqlite_home = true", UnknownCode::InvalidSqliteHomeType),
            ("sqlite_home = ['x']", UnknownCode::InvalidSqliteHomeType),
            ("[sqlite_home]\nx = 1", UnknownCode::InvalidSqliteHomeType),
            (r#"sqlite_home = "a\u0000b""#, UnknownCode::InvalidPath),
            ("sqlite_home = 'a'\nsqlite_home = 'b'", UnknownCode::InvalidToml)] {
            let configs = [ConfigInput { kind: ConfigKind::User, file: &file, text: Observed::Known(text) }];
            let out = known_catalog_locations(&inputs(&home, &configs, SqliteHomeEnv::KnownAbsent));
            assert_eq!(out.diagnostics[0].code, code);
            assert_eq!(out.candidates.len(), 1);
        }
        let configs = [ConfigInput { kind: ConfigKind::User, file: &file,
            text: Observed::Known("password = 'SECRET-CANARY'\nsqlite_home = [") }];
        let out = known_catalog_locations(&inputs(&home, &configs, SqliteHomeEnv::KnownAbsent));
        assert!(!format!("{out:?}").contains("SECRET-CANARY"));
        assert_eq!(out.diagnostics[0].code.code(), "CCL_INVALID_TOML");
    }

    #[test]
    fn config_size_and_structure_limits_precede_toml_errors() {
        let home = p("native-home");
        let file = p("user/config.toml");
        for (text, code) in [("[".repeat(MAX_CONFIG_BYTES + 1), UnknownCode::ConfigByteLimit),
            ("=".repeat(MAX_CONFIG_MARKERS + 1), UnknownCode::ConfigStructureLimit),
            ("[".repeat(MAX_CONFIG_DEPTH + 1), UnknownCode::ConfigStructureLimit),
            ("a.".repeat(MAX_CONFIG_DEPTH + 1), UnknownCode::ConfigStructureLimit)] {
            let configs = [ConfigInput { kind: ConfigKind::User, file: &file, text: Observed::Known(&text) }];
            let out = known_catalog_locations(&inputs(&home, &configs, SqliteHomeEnv::KnownAbsent));
            assert_eq!(out.diagnostics[0].code, code);
        }
        let text = format!("#{}", "x".repeat(MAX_CONFIG_BYTES - 1));
        let configs = [ConfigInput { kind: ConfigKind::User, file: &file, text: Observed::Known(&text) }];
        assert!(known_catalog_locations(&inputs(&home, &configs, SqliteHomeEnv::KnownAbsent)).diagnostics.is_empty());
    }

    #[test]
    fn file_count_and_total_bytes_are_bounded_without_losing_default_or_env() {
        let home = p("native-home");
        let file = p("user/config.toml");
        let configs = (0..MAX_CONFIG_FILES + 1).map(|_| ConfigInput { kind: ConfigKind::User,
            file: &file, text: Observed::Known("[") }).collect::<Vec<_>>();
        let env = p("env-db");
        let out = known_catalog_locations(&inputs(&home, &configs, SqliteHomeEnv::Known(env.to_str().unwrap())));
        assert_eq!(out.candidates.len(), 2);
        assert_eq!(out.diagnostics, [Diagnostic { source: Source::ConfigBatch, code: UnknownCode::ConfigFileLimit }]);
        let text = format!("#{}", "x".repeat(MAX_CONFIG_BYTES - 1));
        let configs = (0..5).map(|_| ConfigInput { kind: ConfigKind::User,
            file: &file, text: Observed::Known(&text) }).collect::<Vec<_>>();
        let out = known_catalog_locations(&inputs(&home, &configs, SqliteHomeEnv::KnownAbsent));
        assert_eq!(out.diagnostics.len(), 1);
        assert_eq!(out.diagnostics[0].source, Source::Config { index: 4, kind: ConfigKind::User });
        assert_eq!(out.diagnostics[0].code, UnknownCode::ConfigByteLimit);
    }

    #[test]
    fn oversized_field_env_metadata_and_join_are_rejected_before_path_allocation() {
        let home = p("native-home");
        let long = "x".repeat(MAX_PATH_BYTES + 1);
        assert_eq!(resolve_path(&long, None, Observed::KnownAbsent,
            UnknownCode::NativeCwdUnavailable), Err(UnknownCode::PathLimit));
        let text = format!("sqlite_home = '{long}'");
        assert_eq!(config_root(&text, &home, Observed::KnownAbsent), Err(UnknownCode::PathLimit));
        let out = known_catalog_locations(&inputs(&home, &[], SqliteHomeEnv::Known(&" ".repeat(MAX_PATH_BYTES + 1))));
        assert_eq!(out.diagnostics[0].code, UnknownCode::PathLimit);
        assert_eq!(absolute_metadata_path(&p(&long), UnknownCode::InvalidPath), Err(UnknownCode::PathLimit));
        assert_eq!(resolve_path(&"x".repeat(MAX_PATH_BYTES), Some(&home), Observed::KnownAbsent,
            UnknownCode::NativeCwdUnavailable), Err(UnknownCode::PathLimit));
    }

    #[test]
    fn missing_home_or_bad_source_does_not_prevent_independent_absolute_env() {
        let home = p("unused");
        let env = p("env-db");
        let configs = [ConfigInput { kind: ConfigKind::User, file: Path::new("relative/config.toml"),
            text: Observed::Known("sqlite_home = 'db'") }];
        let mut input = inputs(&home, &configs, SqliteHomeEnv::Known(env.to_str().unwrap()));
        input.resolved_codex_home = Observed::Unknown(UnknownCode::NotObserved);
        input.os_home = Observed::KnownAbsent;
        let out = known_catalog_locations(&input);
        assert_eq!(out.candidates.len(), 1);
        assert_eq!(out.candidates[0].root, env);
        assert_eq!(out.diagnostics.len(), 2);
        assert_eq!(resolve_path("~//db", None, Observed::KnownAbsent,
            UnknownCode::NativeCwdUnavailable), Err(UnknownCode::OsHomeUnavailable));
        input.resolved_codex_home = Observed::KnownAbsent;
        assert_eq!(known_catalog_locations(&input).diagnostics[0].code, UnknownCode::ResolvedHomeUnavailable);
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_metadata_is_unknown_and_never_lossily_converted() {
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(std::ffi::OsStr::from_bytes(b"/bad-\xff"));
        assert_eq!(absolute_metadata_path(path, UnknownCode::InvalidPath), Err(UnknownCode::InvalidEncoding));
    }

    #[cfg(windows)]
    #[test]
    fn windows_catalog_candidates_share_normalized_verbatim_and_dos_roots() {
        let home = Path::new(r"\\?\C:\profile");
        let file = Path::new(r"\\?\C:\config\..\config.toml");
        let configs = [ConfigInput { kind: ConfigKind::User, file,
            text: Observed::Known("sqlite_home = 'profile'") }];
        let out = known_catalog_locations(&inputs(home, &configs, SqliteHomeEnv::Known(r"C:\profile")));
        assert!(out.diagnostics.is_empty());
        assert_eq!(out.candidates.len(), 1);
        assert_eq!(out.candidates[0].root, PathBuf::from(r"C:\profile"));
        assert_eq!(out.candidates[0].origins.len(), 3);
    }

    #[cfg(windows)]
    #[test]
    fn windows_device_alias_drive_relative_and_tilde_backslash() {
        let base = Path::new(r"C:\base\cwd");
        let home = Path::new(r"C:\Users\fixture");
        for (raw, expected) in [(r"\\?\D:\missing\..\db", r"D:\db"),
            (r"D:db", r"D:\base\cwd\db"), (r"\db", r"C:\db"),
            (r"~\\missing\..\db", r"C:\Users\fixture\db")] {
            assert_eq!(resolve_path(raw, Some(base), Observed::Known(home),
                UnknownCode::NativeCwdUnavailable).unwrap(), PathBuf::from(expected));
        }
    }
}
