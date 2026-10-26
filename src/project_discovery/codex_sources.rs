//! Read bounded catalog-location metadata; author: admin. No Agent execution.
use std::{fs, path::Path};
use crate::config_read::{read_regular_file_bytes, same_regular_file, RegularFileReadError};
use super::codex_config::{self, ConfigInput, ConfigKind, KnownCatalogInputs, KnownCatalogLocations,
    Observed, SqliteHomeEnv, UnknownCode, Diagnostic, Source, SourceObservation, ObservationStatus};

// The text snapshot stays internal and deliberately has no Debug/Serialize.
enum ConfigSnapshot { Absent, Text(String), Unknown(UnknownCode) }

impl ConfigSnapshot {
    fn observed(&self) -> Observed<&str> {
        match self {
            Self::Absent => Observed::KnownAbsent,
            Self::Text(text) => Observed::Known(text),
            Self::Unknown(code) => Observed::Unknown(*code),
        }
    }
}

fn config_snapshot(path: &Path, before_open: impl FnOnce()) -> ConfigSnapshot {
    let before = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return ConfigSnapshot::Unknown(UnknownCode::Unreadable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
            && fs::symlink_metadata(path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {
                return ConfigSnapshot::Absent;
            },
        Err(_) => return ConfigSnapshot::Unknown(UnknownCode::Unreadable),
    };
    let raw = match read_regular_file_bytes(path, before_open) {
        Ok(raw) => raw,
        Err(RegularFileReadError::TooLarge) => return ConfigSnapshot::Unknown(UnknownCode::ConfigByteLimit),
        Err(_) => return ConfigSnapshot::Unknown(UnknownCode::Unreadable),
    };
    let current = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return ConfigSnapshot::Unknown(UnknownCode::Unreadable),
    };
    if !same_regular_file(&before, &raw.opened) || !same_regular_file(&raw.opened, &raw.finished)
        || !same_regular_file(&raw.finished, &current) {
        return ConfigSnapshot::Unknown(UnknownCode::Unreadable);
    }
    match String::from_utf8(raw.bytes) {
        Ok(text) => ConfigSnapshot::Text(text),
        Err(_) => ConfigSnapshot::Unknown(UnknownCode::InvalidEncoding),
    }
}

/// A caller supplied catalog root is inspected without inheriting external
/// config/env. This is a reader scope, not a native effective-config claim.
pub(super) fn explicit_locations(home: &Path) -> KnownCatalogLocations {
    codex_config::known_catalog_locations(&KnownCatalogInputs {
        os_home: Observed::KnownAbsent, resolved_codex_home: Observed::Known(home),
        native_resolved_cwd: None, configs: &[], sqlite_home_env: SqliteHomeEnv::KnownAbsent,
    })
}

pub(super) fn runtime_locations(home: Option<&Path>, inspector: bool) -> KnownCatalogLocations {
    // A dedicated reader override cannot inherit unrelated external catalogs.
    if inspector {
        return codex_config::known_catalog_locations(&KnownCatalogInputs {
            os_home: Observed::KnownAbsent,
            resolved_codex_home: home.map(Observed::Known).unwrap_or(Observed::Unknown(UnknownCode::ResolvedHomeUnavailable)),
            native_resolved_cwd: None, configs: &[], sqlite_home_env: SqliteHomeEnv::KnownAbsent,
        });
    }
    let user_path = home.map(|home| home.join("config.toml"));
    #[cfg(unix)]
    let system_path = Some(Path::new("/etc/codex/config.toml"));
    #[cfg(windows)]
    let system_location = super::codex_windows::system_config_path();
    #[cfg(windows)]
    let system_path = match &system_location {
        Ok(path) => Some(path.as_path()),
        Err(error) => {
            // Fixed code only; no OS error text, path or configuration data.
            eprintln!("Codex System 位置未知：{}", error.code());
            None
        },
    };
    #[cfg(not(any(unix, windows)))]
    let system_path: Option<&Path> = None;
    let os_home = dirs::home_dir();
    let env = std::env::var("CODEX_SQLITE_HOME");
    let sqlite_home_env = match &env {
        Ok(value) => SqliteHomeEnv::Known(value),
        Err(std::env::VarError::NotPresent) => SqliteHomeEnv::KnownAbsent,
        Err(std::env::VarError::NotUnicode(_)) => SqliteHomeEnv::NonUnicode,
    };
    // Independent System/env observations do not depend on a known User home.
    locations_for_config_files(home, os_home.as_deref(), system_path, user_path.as_deref(), sqlite_home_env, None)
}

/// Same bounded file and parser path as production, with explicit native
/// metadata inputs so fixtures never inherit the user's configuration/env.
pub(super) fn locations_from_config_files(
    home: &Path, os_home: Option<&Path>, system_path: Option<&Path>, user_path: &Path,
    sqlite_home_env: SqliteHomeEnv<'_>, native_resolved_cwd: Option<&Path>,
) -> KnownCatalogLocations {
    locations_for_config_files(Some(home), os_home, system_path, Some(user_path), sqlite_home_env, native_resolved_cwd)
}

pub(super) fn locations_for_config_files(
    home: Option<&Path>, os_home: Option<&Path>, system_path: Option<&Path>, user_path: Option<&Path>,
    sqlite_home_env: SqliteHomeEnv<'_>, native_resolved_cwd: Option<&Path>,
) -> KnownCatalogLocations {
    let user = user_path.map(|path| config_snapshot(path, || {}));
    let system = system_path.map(|path| config_snapshot(path, || {}));
    let mut configs = Vec::new();
    if let Some((file, snapshot)) = system_path.zip(system.as_ref()) {
        configs.push(ConfigInput { kind: ConfigKind::System, file, text: snapshot.observed() });
    }
    if let Some((file, snapshot)) = user_path.zip(user.as_ref()) {
        configs.push(ConfigInput { kind: ConfigKind::User, file, text: snapshot.observed() });
    }
    let mut locations = codex_config::known_catalog_locations(&KnownCatalogInputs {
        os_home: os_home.map(Observed::Known).unwrap_or(Observed::KnownAbsent),
        resolved_codex_home: home.map(Observed::Known).unwrap_or(Observed::Unknown(UnknownCode::ResolvedHomeUnavailable)),
        native_resolved_cwd, configs: &configs, sqlite_home_env,
    });
    // Omitted locations are unobserved, not proof that the config is absent.
    let mut index = configs.len();
    for (kind, missing) in [(ConfigKind::System, system_path.is_none()), (ConfigKind::User, user_path.is_none())] {
        if missing {
            let source = Source::Config { index, kind };
            let code = if kind == ConfigKind::User && home.is_none() {
                UnknownCode::ResolvedHomeUnavailable
            } else { UnknownCode::NotObserved };
            locations.observations.push(SourceObservation { source, status: ObservationStatus::Unknown(code) });
            locations.diagnostics.push(Diagnostic { source, code });
            index += 1;
        }
    }
    locations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unresolved_home_keeps_independent_system_and_absolute_environment_candidates() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let system = base.join("system.toml");
        fs::write(&system, "sqlite_home = 'system-data'\n").unwrap();
        let env = base.join("env-data");
        let result = locations_for_config_files(None, Some(&base), Some(&system), None,
            SqliteHomeEnv::Known(env.to_str().unwrap()), None);
        assert_eq!(result.candidates.iter().map(|candidate| candidate.root.clone()).collect::<Vec<_>>(),
            [base.join("system-data"), env]);
        assert!(result.diagnostics.iter().any(|item| item.code == UnknownCode::ResolvedHomeUnavailable));
        assert!(result.observations.iter().any(|item| matches!(item.source,
            Source::Config { kind: ConfigKind::User, .. }) && item.status == ObservationStatus::Unknown(UnknownCode::ResolvedHomeUnavailable)));
    }

    #[test]
    fn snapshot_missing_dangling_directory_and_encoding_keep_distinct_states() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing.toml");
        assert!(matches!(config_snapshot(&missing, || {}), ConfigSnapshot::Absent));
        assert!(matches!(config_snapshot(root.path(), || {}), ConfigSnapshot::Unknown(UnknownCode::Unreadable)));
        let file = root.path().join("invalid.toml");
        fs::write(&file, [0xff]).unwrap();
        assert!(matches!(config_snapshot(&file, || {}), ConfigSnapshot::Unknown(UnknownCode::InvalidEncoding)));
        #[cfg(unix)] {
            let alias = root.path().join("dangling.toml");
            std::os::unix::fs::symlink(missing, &alias).unwrap();
            assert!(matches!(config_snapshot(&alias, || {}), ConfigSnapshot::Unknown(UnknownCode::Unreadable)));
        }
    }

    #[test]
    fn snapshot_rejects_oversize_without_exposing_contents() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("config.toml");
        fs::File::create(&file).unwrap().set_len(codex_config::MAX_CONFIG_BYTES as u64 + 1).unwrap();
        assert!(matches!(config_snapshot(&file, || {}), ConfigSnapshot::Unknown(UnknownCode::ConfigByteLimit)));
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_follows_regular_alias_and_rejects_open_time_fifo_replacement() {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("config.toml");
        let alias = root.path().join("alias.toml");
        fs::write(&file, "sqlite_home = '/owned/catalog'\n").unwrap();
        symlink(&file, &alias).unwrap();
        assert!(matches!(config_snapshot(&alias, || {}), ConfigSnapshot::Text(text) if text.contains("sqlite_home")));
        let result = config_snapshot(&file, || {
            fs::remove_file(&file).unwrap();
            let path = std::ffi::CString::new(file.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        });
        assert!(matches!(result, ConfigSnapshot::Unknown(UnknownCode::Unreadable)));
    }
}
