//! Grok home routing, without directory creation or a process-wide cache.
//!
//! Pinned source: xai-org/grok-build@2bdd1d6a, xai-dirs/src/lib.rs:41-58.
//! Absolute overrides are retained verbatim. Empty overrides fall back to home.
//! Relative overrides (including literal `~`) are deliberately unsupported:
//! installers must not silently select a cwd-dependent destination.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};

pub(crate) mod manual;

/// Freshly sample only GROK_HOME. The supplied home belongs to the caller;
/// this function never discovers an account home or creates any directory.
pub(crate) fn grok_config_root(home: &Path) -> Result<PathBuf> {
    grok_config_root_from(home, std::env::var_os("GROK_HOME").as_deref())
}

/// Pure, injectable routing. Native canonicalizes the default account home;
/// callers needing the same path spelling should supply that canonical home.
/// No trimming, tilde expansion, environment expansion or I/O occurs here.
pub(crate) fn grok_config_root_from(
    home: &Path,
    grok_home: Option<&OsStr>,
) -> Result<PathBuf> {
    let selected = match grok_home.filter(|value| !value.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => home.join(".grok"),
    };
    ensure!(
        selected.is_absolute() && !selected.as_os_str().as_encoded_bytes().contains(&0),
        "Grok 配置根不是受支持的绝对路径，状态未知"
    );
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_and_empty_use_the_supplied_home_without_io() {
        let home = &std::env::temp_dir().join("synthetic-home");
        for value in [None, Some(OsStr::new(""))] {
            assert_eq!(grok_config_root_from(home, value).unwrap(), home.join(".grok"));
        }
    }

    #[test]
    fn absolute_override_is_not_trimmed_or_normalized() {
        let path = std::env::temp_dir().join("synthetic custom/../grok ");
        let value = path.as_os_str();
        assert_eq!(
            grok_config_root_from(Path::new("relative-unused-home"), Some(value)).unwrap(),
            PathBuf::from(value)
        );
    }

    #[test]
    fn unsupported_paths_return_only_a_fixed_error() {
        let invalid = std::env::temp_dir().join("SECRET\0invalid");
        for value in ["relative-SECRET", "~/SECRET", " ", invalid.to_str().unwrap()] {
            let error = grok_config_root_from(&std::env::temp_dir().join("home"), Some(OsStr::new(value)))
                .unwrap_err()
                .to_string();
            assert_eq!(error, "Grok 配置根不是受支持的绝对路径，状态未知");
        }
        assert!(grok_config_root_from(Path::new("relative"), None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_absolute_override_is_preserved() {
        use std::os::unix::ffi::OsStrExt;
        let value = OsStr::from_bytes(b"/synthetic/\xff");
        assert_eq!(
            grok_config_root_from(Path::new("/home"), Some(value)).unwrap(),
            PathBuf::from(value)
        );
    }
}
