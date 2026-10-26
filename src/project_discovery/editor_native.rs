//! Native editor metadata only; version evidence: editor-native-storage.json.
//! TRAE CN 3.4.4 (cn.trae.app), archive SHA256:
//! f620d29eca1d24e222c8b4ed1740b01257794007f5392759fd085af748a6d52a.
//! Antigravity IDE 2.5.5 (com.google.antigravity-ide), archive SHA256:
//! cad67d6d30a537fcb3bb9a6100330040ec6bcb686e38069f1ff16fe42306a33e.
//! This does NOT support Antigravity App 2.24.0, CLI, or international TRAE.
//! Defaults below are macOS userData roots; other roots must be explicit.
//! The shared reader owns bounds, metadata projections, JSON5 and sqlite_vfs.
//! No native archive flag exists in these verified history schemas: do not
//! infer a native flag: every archive observation is None (unknown), and
//! archived=false is only the display default. The caller preserves known
//! per-source observations and manual hidden /
//! retained overlays and retains previous source state on Err.
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};

use super::{AgentKind, ProjectEntry};

/// Kitter-selected TRAE CN userData override; never the international edition.
pub(super) fn trae_projects(home: &Path) -> Result<Vec<ProjectEntry>> {
    let root = user_data_root(
        home,
        std::env::var_os("KITTER_TRAE_PROJECTS_USER_DATA").as_deref(),
        "Trae CN",
    )?;
    trae_projects_at(&root)
}

/// Explicit TRAE CN userData root, independent of process environment.
pub(super) fn trae_projects_at(user_data: &Path) -> Result<Vec<ProjectEntry>> {
    editor_projects_at(user_data, AgentKind::Trae)
}

/// Standalone IDE only. This override must not point to Antigravity App state.
pub(super) fn antigravity_projects(home: &Path) -> Result<Vec<ProjectEntry>> {
    let root = user_data_root(
        home,
        std::env::var_os("KITTER_ANTIGRAVITY_PROJECTS_USER_DATA").as_deref(),
        "Antigravity IDE",
    )?;
    antigravity_projects_at(&root)
}

/// Explicit Antigravity IDE userData root; no App / CLI fallback.
pub(super) fn antigravity_projects_at(user_data: &Path) -> Result<Vec<ProjectEntry>> {
    editor_projects_at(user_data, AgentKind::Antigravity)
}

fn editor_projects_at(user_data: &Path, agent: AgentKind) -> Result<Vec<ProjectEntry>> {
    absolute_root(user_data)?;
    // An existing userData root alone does not prove this storage protocol.
    // Missing both verified metadata sources must retain the previous Agent
    // observations through native::refresh, rather than clear them as empty.
    let history = user_data.join("User/globalStorage/state.vscdb");
    let storage = user_data.join("User/workspaceStorage");
    let history_present = history
        .try_exists()
        .map_err(|_| anyhow!("编辑器项目登记暂不可读"))?;
    let storage_present = storage
        .try_exists()
        .map_err(|_| anyhow!("编辑器工作区登记暂不可读"))?;
    if !history_present && !storage_present {
        bail!("编辑器原生项目登记缺失或协议未知");
    }
    super::vscode::editor_projects_at(user_data, agent)
}

fn absolute_root(root: &Path) -> Result<()> {
    if !root.is_absolute() {
        bail!("编辑器项目数据根必须是非空绝对路径");
    }
    Ok(())
}

fn user_data_root(home: &Path, selected: Option<&OsStr>, product: &str) -> Result<PathBuf> {
    let root = match selected {
        Some(root) => PathBuf::from(root),
        None => {
            if !cfg!(target_os = "macos") {
                bail!("此编辑器的默认发现仅支持 macOS，请显式指定 userData 根");
            }
            home.join("Library/Application Support").join(product)
        }
    };
    absolute_root(&root)?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;
    use std::{collections::BTreeSet, fs};

    type Reader = fn(&Path) -> Result<Vec<ProjectEntry>>;
    const READERS: [(AgentKind, Reader); 2] = [
        (AgentKind::Trae, trae_projects_at),
        (AgentKind::Antigravity, antigravity_projects_at),
    ];

    fn uri(path: &Path) -> String {
        format!(
            "file://{}",
            path.to_str()
                .unwrap()
                .replace('%', "%25")
                .replace(' ', "%20")
                .replace('#', "%23")
        )
    }

    // RW SQLite is used only to create our synthetic fixture, never to read
    // source data. The production path delegates to the write-denying VFS.
    fn database(root: &Path, history: &str) -> PathBuf {
        let db = root.join("User/globalStorage/state.vscdb");
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE ItemTable(key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
                 INSERT INTO ItemTable VALUES('glass.localAgentProjects.v1', 'invalid-json');
                 INSERT INTO ItemTable VALUES('chat.fixture', 'not-a-project');",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO ItemTable VALUES('history.recentlyOpenedPathsList', ?1)",
                [history],
            )
            .unwrap();
        db
    }

    #[test]
    fn overrides_are_absolute_and_invalid_values_never_fall_back() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        for product in ["Trae CN", "Antigravity IDE"] {
            for invalid in ["", "relative", "~/editor"] {
                assert!(user_data_root(&home, Some(OsStr::new(invalid)), product).is_err());
            }
            let explicit = home.join("selected");
            assert_eq!(
                user_data_root(
                    Path::new("relative-home"),
                    Some(explicit.as_os_str()),
                    product
                )
                .unwrap(),
                explicit
            );
            assert!(user_data_root(Path::new("relative-home"), None, product).is_err());
            if cfg!(target_os = "macos") {
                assert_eq!(
                    user_data_root(&home, None, product).unwrap(),
                    home.join("Library/Application Support").join(product)
                );
            } else {
                assert!(user_data_root(&home, None, product).is_err());
            }
        }
        for (_, read) in READERS {
            assert!(read(Path::new("")).is_err());
            assert!(read(Path::new("relative")).is_err());
        }
    }

    #[test]
    fn both_editors_keep_native_multiroot_mapping_and_ignore_cursor_only_state() {
        for (agent, read) in READERS {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let data = root.join("selected-user-data");
            let one = root.join("中文 one%#");
            let two = root.join("two");
            let sidecar_only = root.join("sidecar-only");
            for path in [&one, &two, &sidecar_only] {
                fs::create_dir(path).unwrap();
            }
            let config = root.join("native.code-workspace");
            let config_text = format!(
                "{{ // native JSON5\n folders:[{{path:'中文 one%#'}},{{uri:{}}},], settings:{{ignored:true}} }}",
                serde_json::to_string(&uri(&two)).unwrap()
            );
            fs::write(&config, &config_text).unwrap();
            let db = database(
                &data,
                &json!({"entries":[
                    {"folderUri":uri(&one)},
                    {"workspace":{"id":"native-workspace","configPath":uri(&config)}},
                    {"fileUri":uri(&root)},
                    {"folderUri":uri(&root),"remoteAuthority":"ssh-remote+fixture"},
                    {"folderUri":"vscode-remote://ssh-remote+fixture/tmp"}
                ]})
                .to_string(),
            );
            let side = data.join("User/workspaceStorage/sidecar-id");
            fs::create_dir_all(&side).unwrap();
            let metadata = json!({"folder":uri(&sidecar_only)}).to_string();
            fs::write(side.join("workspace.json"), &metadata).unwrap();
            fs::write(side.join("state.vscdb"), "not-a-database-do-not-read").unwrap();
            let before = fs::read(&db).unwrap();
            let projects = read(&data).unwrap();
            assert_eq!(projects.len(), 3);
            assert_eq!(
                projects
                    .iter()
                    .map(|entry| entry.path.clone())
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([one.clone(), two.clone(), sidecar_only.clone()])
            );
            let members = BTreeSet::from([one.clone(), two.clone()]);
            for project in &projects {
                assert_eq!(project.agents, [agent]);
                assert!(
                    project
                        .archive_states
                        .iter()
                        .all(|state| state.archived.is_none()),
                    "history / sidecar observations must remain unknown"
                );
                assert!(!project.archived, "unknown uses the display default only");
                if members.contains(&project.path) {
                    assert_eq!(project.native_workspaces.len(), 1);
                    let workspace = &project.native_workspaces[0];
                    assert_eq!(workspace.agent, agent);
                    assert_eq!(workspace.path, config);
                    assert_eq!(
                        workspace
                            .project_roots
                            .iter()
                            .cloned()
                            .collect::<BTreeSet<_>>(),
                        members
                    );
                } else {
                    assert!(project.native_workspaces.is_empty());
                }
            }
            // Discovery is neutral; existing manual hide / restore sets remain
            // owned by the parent, including when metadata has no archive flag.
            let hidden = BTreeSet::from([one.clone()]);
            let retained = BTreeSet::from([two.clone()]);
            let merged = super::super::all_projects(&[], &projects, &hidden, &retained);
            assert!(
                merged
                    .iter()
                    .find(|entry| entry.path == one)
                    .unwrap()
                    .hidden
            );
            assert!(
                merged
                    .iter()
                    .find(|entry| entry.path == two)
                    .unwrap()
                    .retained
            );
            assert_eq!(fs::read(&db).unwrap(), before);
            assert_eq!(fs::read_to_string(&config).unwrap(), config_text);
            assert_eq!(
                fs::read_to_string(side.join("workspace.json")).unwrap(),
                metadata
            );
            assert!(!db.with_extension("vscdb-journal").exists());
        }
    }

    #[test]
    fn unreadable_or_invalid_sources_return_errors_instead_of_empty_success() {
        for (_, read) in READERS {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            assert!(read(&root.join("missing")).is_err());
            let db = database(&root, "invalid-history-json");
            assert!(read(&root).is_err());
            fs::write(db, "invalid-sqlite").unwrap();
            assert!(read(&root).is_err());
        }
    }

    #[test]
    fn absent_native_metadata_is_unknown_but_explicit_empty_history_is_valid() {
        for (_, read) in READERS {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            // Skill trees and App UI storage are not editor registrations.
            fs::create_dir_all(root.join(".agents/skills/fixture")).unwrap();
            fs::write(root.join("app_storage.json"), "{}").unwrap();
            assert!(read(&root).is_err());
            assert!(!root.join("User").exists());
            database(&root, "{\"entries\":[]}");
            assert!(read(&root).unwrap().is_empty());
        }
    }

    #[test]
    fn duplicate_roots_keep_distinct_workspace_and_agent_identities() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let cwd = root.join("project");
        fs::create_dir(&cwd).unwrap();
        let config = root.join("one.code-workspace");
        let other_config = root.join("two.code-workspace");
        for path in [&config, &other_config] {
            fs::write(
                path,
                "{\"folders\":[{\"path\":\"project\"},{\"path\":\"project/.\"}]}",
            )
            .unwrap();
        }
        #[cfg(unix)]
        let alias = {
            let alias = root.join("project-alias");
            std::os::unix::fs::symlink(&cwd, &alias).unwrap();
            alias
        };
        #[cfg(not(unix))]
        let alias = cwd.join(".");
        let data = root.join("data");
        database(
            &data,
            &json!({"entries":[
                {"folderUri":uri(&cwd)}, {"folderUri":uri(&alias)},
                {"workspace":{"id":"one","configPath":uri(&config)}},
                {"workspace":{"id":"duplicate-reference","configPath":uri(&config)}},
                {"workspace":{"id":"two","configPath":uri(&other_config)}}
            ]})
            .to_string(),
        );
        let mut combined = Vec::new();
        let mut ids = BTreeSet::new();
        for (agent, read) in READERS {
            let projects = read(&data).unwrap();
            assert_eq!(projects.len(), 1);
            let project = &projects[0];
            assert_eq!(project.path, cwd);
            assert_eq!(project.agents, [agent]);
            assert_eq!(project.native_workspaces.len(), 2);
            for workspace in &project.native_workspaces {
                assert_eq!(workspace.agent, agent);
                assert_eq!(workspace.project_roots.as_slice(), [cwd.clone()]);
                assert_eq!(workspace.archived, None);
                assert!(ids.insert(workspace.id()));
            }
            combined.extend(projects);
        }
        assert_eq!(ids.len(), 4, "config identity includes the owning Agent");
        let merged = super::super::all_projects(&[], &combined, &BTreeSet::new(), &BTreeSet::new());
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].agents.len(), 2);
        assert_eq!(merged[0].native_workspaces.len(), 4);
        assert!(
            merged[0]
                .archive_states
                .iter()
                .all(|state| state.archived.is_none())
        );
    }

    #[test]
    fn stale_non_directory_remote_and_unregistered_paths_are_not_projects() {
        for (agent, read) in READERS {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let cwd = root.join("project");
            let unregistered = root.join("unregistered");
            for path in [&cwd, &unregistered] {
                fs::create_dir(path).unwrap();
            }
            let ordinary_file = root.join("ordinary-file");
            fs::write(&ordinary_file, "fixture").unwrap();
            let config = root.join("registered.code-workspace");
            fs::write(
                &config,
                json!({"folders":[
                    {"path":"project"}, {"path":"missing"}, {"path":"ordinary-file"},
                    {"uri":"vscode-remote://ssh-remote+fixture/tmp"}
                ]})
                .to_string(),
            )
            .unwrap();
            fs::write(
                root.join("not-registered.code-workspace"),
                "{\"folders\":[{\"path\":\"unregistered\"}]}",
            )
            .unwrap();
            let data = root.join("data");
            database(
                &data,
                &json!({"entries":[
                    {"folderUri":uri(&root.join("missing"))},
                    {"folderUri":uri(&ordinary_file)},
                    {"folderUri":"file://remote-host/tmp"},
                    {"folderUri":"file:relative"},
                    {"fileUri":uri(&unregistered)},
                    {"workspace":{"id":"gone","configPath":uri(&root.join("gone.code-workspace"))}},
                    {"workspace":{"id":"registered","configPath":uri(&config)}}
                ]})
                .to_string(),
            );
            let projects = read(&data).unwrap();
            assert_eq!(projects.len(), 1);
            assert_eq!(projects[0].path, cwd);
            assert_eq!(projects[0].agents, [agent]);
            assert_eq!(projects[0].native_workspaces[0].path, config);
        }
    }

    #[test]
    fn sidecar_only_multiroot_is_supported_and_unproven_archive_flags_are_ignored() {
        for (agent, read) in READERS {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let cwd = root.join("project");
            fs::create_dir(&cwd).unwrap();
            let config = root.join("registered.code-workspace");
            fs::write(
                &config,
                "{\"folders\":[{\"path\":\"project\"}],\"archived\":true}",
            )
            .unwrap();
            let data = root.join("data");
            let side = data.join("User/workspaceStorage/native-id");
            fs::create_dir_all(&side).unwrap();
            fs::write(
                side.join("workspace.json"),
                json!({
                    "workspace":uri(&config), "archived":true, "isArchived":true
                })
                .to_string(),
            )
            .unwrap();
            let projects = read(&data).unwrap();
            assert_eq!(projects.len(), 1);
            assert_eq!(projects[0].agents, [agent]);
            assert_eq!(projects[0].path, cwd);
            assert!(!projects[0].archived);
            assert!(
                projects[0]
                    .archive_states
                    .iter()
                    .all(|state| state.archived.is_none())
            );
            assert_eq!(projects[0].native_workspaces[0].archived, None);
            database(
                &data,
                &json!({"entries":[{
                    "folderUri":uri(&cwd), "archived":true, "isArchived":true
                }]})
                .to_string(),
            );
            assert!(!read(&data).unwrap()[0].archived);
            assert!(!side.join("state.vscdb").exists());
        }
    }

    #[test]
    fn malformed_metadata_fails_without_reading_or_reporting_unrelated_content() {
        for (_, read) in READERS {
            for history in ["{\"entries\":{}}", "{\"entries\":[{\"folderUri\":42}]}"] {
                let temp = tempfile::tempdir().unwrap();
                let root = temp.path().canonicalize().unwrap();
                database(&root, history);
                assert!(read(&root).is_err());
            }
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let side = root.join("User/workspaceStorage/native-id");
            fs::create_dir_all(&side).unwrap();
            fs::write(side.join("workspace.json"), "fixture-invalid-secret").unwrap();
            let error = read(&root).unwrap_err().to_string();
            assert!(!error.contains("fixture-invalid-secret"));
            let config = root.join("invalid.code-workspace");
            fs::write(&config, "{ folders: 'fixture-invalid-secret' }").unwrap();
            fs::write(
                side.join("workspace.json"),
                json!({"workspace":uri(&config)}).to_string(),
            )
            .unwrap();
            let error = read(&root).unwrap_err().to_string();
            assert!(!error.contains("fixture-invalid-secret"));
        }
    }
}
