//! Detect software installations, never shared skills or leftover settings.
use std::{env, path::{Path, PathBuf}};

use super::{AGENT_ICON_ORDER, AgentIconInfo};

pub fn installed_agents() -> Vec<AgentIconInfo> {
    let home = dirs::home_dir().unwrap_or_default();
    let bins = command_roots(&home);
    let apps = [PathBuf::from("/Applications"), home.join("Applications")];
    detect(&bins, &apps)
}

fn command_roots(home: &Path) -> Vec<PathBuf> {
    let mut bins = env::var_os("PATH").map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    #[cfg(unix)]
    {
        bins.extend([PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")]);
        bins.extend([".local/bin", ".npm-global/bin", ".bun/bin", ".cargo/bin",
            ".opencode/bin", ".claude/local", ".pi/agent/bin", ".grok/bin"]
            .map(|relative| home.join(relative)));
    }
    bins
}

/// Resolve a native npm package's bundled root from its installed CLI link,
/// without invoking Node/OpenClaw or guessing a sibling repository's skills.
pub(crate) fn openclaw_bundled_skills_dir(home: &Path) -> Option<PathBuf> {
    bundled_openclaw_package(&command_roots(home))
}

fn bundled_openclaw_package(bins: &[PathBuf]) -> Option<PathBuf> {
    for bin in bins {
        let cli = bin.join(if cfg!(windows) { "openclaw.cmd" } else { "openclaw" });
        if !executable(&cli) { continue; }
        let Some(cli) = cli.canonicalize().ok() else { continue; };
        for root in cli.ancestors().skip(1).take(6) {
            let manifest = root.join("package.json");
            if !manifest.metadata().is_ok_and(|m| m.is_file() && m.len() <= 128 * 1024) { continue; }
            let package = std::fs::read(&manifest).ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
            if package.as_ref().and_then(|v| v.get("name")).and_then(|v| v.as_str()) == Some("openclaw") {
                return root.join("skills").is_dir().then(|| root.join("skills"));
            }
        }
        // The first executable is the active PATH command; later installs do
        // not supply its missing package metadata.
        return None;
    }
    None
}

fn commands(id: &str) -> &'static [&'static str] {
    match id {
        "codex" => &["codex"],
        "claude-code" => &["claude"],
        "cursor" => &["cursor", "cursor-agent"],
        "opencode" => &["opencode"],
        "pi" => &["pi"],
        "proma" => &["proma"],
        "omp" => &["omp"],
        "dimagent" => &["dim", "dimagent"],
        "grok" => &["grok"],
        "openclaw" => &["openclaw"],
        "hermes" => &["hermes"],
        "droid" => &["droid"],
        "amp" => &["amp"],
        "antigravity" => &["agy", "antigravity"],
        "copilot" => &["copilot"],
        "trae" => &["trae"],
        "workbuddy" => &["workbuddy"],
        _ => &[],
    }
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else { return false; };
    if !metadata.is_file() { return false; }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    { true }
}

fn command_installed(name: &str, bins: &[PathBuf]) -> bool {
    bins.iter().any(|dir| {
        #[cfg(windows)]
        {
            ["exe", "cmd", "bat", "com"].iter()
                .any(|extension| executable(&dir.join(format!("{name}.{extension}"))))
        }
        #[cfg(not(windows))]
        { executable(&dir.join(name)) }
    })
}

#[cfg(target_os = "macos")]
fn app_installed(id: &str, roots: &[PathBuf]) -> bool {
    let names: &[&str] = match id {
        "codex" => &["Codex.app", "ChatGPT.app"],
        "claude-code" => &["Claude Code.app", "Claude.app"],
        "cursor" => &["Cursor.app"],
        "opencode" => &["OpenCode.app", "opencode.app"],
        "proma" => &["Proma.app"],
        "dimagent" => &["DimAgent.app"],
        "antigravity" => &["Antigravity.app"],
        "trae" => &["Trae.app", "Trae CN.app"],
        "workbuddy" => &["WorkBuddy.app"],
        _ => &[],
    };
    roots.iter().any(|root| names.iter().any(|name| {
        let bundle = root.join(name);
        let info = bundle.join("Contents/Info.plist");
        if !info.is_file() { return false; }
        let property = |key: &str| -> Option<String> {
            let output = std::process::Command::new("/usr/bin/plutil")
                .args(["-extract", key, "raw", "-o", "-"]).arg(&info).output().ok()?;
            output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        };
        // The ordinary ChatGPT app is not the Codex desktop app.
        if id == "codex" && property("CFBundleIdentifier").as_deref() != Some("com.openai.codex") {
            return false;
        }
        if id == "workbuddy" && property("CFBundleIdentifier").as_deref() != Some("com.tencent.workbuddy.mac") {
            return false;
        }
        let Some(binary) = property("CFBundleExecutable") else { return false; };
        let mut components = Path::new(&binary).components();
        if !matches!(components.next(), Some(std::path::Component::Normal(_)))
            || components.next().is_some() { return false; }
        executable(&bundle.join("Contents/MacOS").join(binary))
    }))
}

fn detect(bins: &[PathBuf], apps: &[PathBuf]) -> Vec<AgentIconInfo> {
    #[cfg(not(target_os = "macos"))]
    let _ = apps;
    let mut agents = AGENT_ICON_ORDER.iter().copied().filter(|agent| {
        if commands(agent.id).iter().any(|name| command_installed(name, bins)) { return true; }
        #[cfg(target_os = "macos")]
        { app_installed(agent.id, apps) }
        #[cfg(not(target_os = "macos"))]
        { false }
    }).collect::<Vec<_>>();
    agents.sort_by_key(|agent| super::display_sort_key(agent.name, true));
    agents
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_executable(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn openclaw_bundled_root_requires_the_active_clis_package_identity() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("package");
        let bins = temp.path().join("bin");
        fs::create_dir_all(package.join("skills")).unwrap();
        fs::create_dir_all(&bins).unwrap();
        write_executable(&package.join("openclaw.mjs"));
        std::os::unix::fs::symlink(package.join("openclaw.mjs"), bins.join("openclaw")).unwrap();
        fs::write(package.join("package.json"), r#"{"name":"unrelated"}"#).unwrap();
        assert!(bundled_openclaw_package(&[bins.clone()]).is_none());
        fs::write(package.join("package.json"), r#"{"name":"openclaw"}"#).unwrap();
        assert_eq!(bundled_openclaw_package(&[bins.clone()]), Some(package.canonicalize().unwrap().join("skills")));
        let earlier = temp.path().join("earlier");
        write_executable(&earlier.join("openclaw"));
        assert!(bundled_openclaw_package(&[earlier, bins]).is_none());
    }

    #[test]
    fn directories_helpers_and_settings_are_not_agent_installations() {
        let temp = tempfile::tempdir().unwrap();
        for relative in [".agents/skills", ".cursor/skills", ".pi/agent", "bin/opencode", "apps/Cursor.app"] {
            fs::create_dir_all(temp.path().join(relative)).unwrap();
        }
        for helper in ["fd", "rg", "gh", "opencodex"] { write_executable(&temp.path().join("bin").join(helper)); }
        fs::write(temp.path().join("bin/pi"), "not executable").unwrap();
        assert!(detect(&[temp.path().join("bin")], &[temp.path().join("apps")]).is_empty());
    }

    #[test]
    fn executable_agents_are_ordered_and_broken_links_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let suffix = ".cmd";
        #[cfg(not(windows))]
        let suffix = "";
        for name in ["grok", "codex", "agy", "dim"] {
            write_executable(&temp.path().join(format!("{name}{suffix}")));
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(temp.path().join("missing"), temp.path().join("opencode")).unwrap();
        let installed = detect(&[temp.path().to_path_buf()], &[]);
        assert_eq!(installed.iter().map(|agent| agent.id).collect::<Vec<_>>(), ["antigravity", "codex", "dimagent", "grok"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn desktop_only_apps_require_their_real_executable_and_codex_identity() {
        let temp = tempfile::tempdir().unwrap();
        for (name, id) in [("Proma", "cool.proma.app"), ("ChatGPT", "com.openai.chat") ] {
            let bundle = temp.path().join(format!("{name}.app"));
            write_executable(&bundle.join("Contents/MacOS").join(name));
            fs::write(bundle.join("Contents/Info.plist"), format!(
                "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>{id}</string><key>CFBundleExecutable</key><string>{name}</string></dict></plist>"
            )).unwrap();
        }
        let apps = [temp.path().to_path_buf()];
        assert_eq!(detect(&[], &apps).iter().map(|agent| agent.id).collect::<Vec<_>>(), ["proma"]);
        let info = temp.path().join("ChatGPT.app/Contents/Info.plist");
        let contents = fs::read_to_string(&info).unwrap().replace("com.openai.chat", "com.openai.codex");
        fs::write(info, contents).unwrap();
        assert_eq!(detect(&[], &apps).iter().map(|agent| agent.id).collect::<Vec<_>>(), ["codex", "proma"]);
        fs::remove_file(temp.path().join("Proma.app/Contents/MacOS/Proma")).unwrap();
        assert!(!app_installed("proma", &apps));
    }
}
