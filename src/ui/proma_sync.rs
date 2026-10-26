use super::*;

impl KitterApp {
    pub(super) fn start_local_proma_sync(&mut self, cx: &mut Context<Self>) {
        let Some(root) = crate::proma::home() else {
            return;
        };
        let bundled = crate::proma::builtin_sync::bundled_dir();
        let builtin_state = crate::config::app_data_dir().join("proma-builtin-follow");
        self.model.proma_sync_task = Some(cx.spawn(async move |this, cx| {
            let mut sync = crate::proma::LocalMirrorSync::default();
            let mut builtins = crate::proma::builtin_sync::BuiltinFollower::default();
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                let Ok(sources) =
                    this.update(cx, |this, cx| {
                        if this.shell.dialog_body.as_ref().is_some_and(|body| matches!(body.read(cx).kind, DialogKind::Storage)) {
                            return None;
                        }
                        this.model.proma_sync_running = true;
                        Some(this.model.library.local_source_paths())
                    })
                else {
                    break;
                };
                let Some(sources) = sources else { continue; };
                let root = root.clone();
                let bundled = bundled.clone();
                let builtin_state = builtin_state.clone();
                let (returned_sync, returned_builtins, report) = cx
                    .background_executor()
                    .spawn(async move {
                        let mut report = sync.poll(&root, &sources).unwrap_or_else(|error| {
                            crate::proma::LocalSyncReport {
                                warnings: vec![format!("{error}")],
                                ..Default::default()
                            }
                        });
                        if let Some(bundled) = bundled {
                            match builtins.poll(&root, &bundled, &builtin_state) {
                                Ok(builtin_report) => {
                                    report.updated += builtin_report.updated;
                                    report.inactive_updated += builtin_report.inactive_updated;
                                    report.catalog_changed |= builtin_report.catalog_changed;
                                    report.warnings.extend(builtin_report.warnings);
                                }
                                Err(error) => report.warnings.push(format!("Proma 内置技能：{error}")),
                            }
                        }
                        report.warnings.sort();
                        report.warnings.dedup();
                        (sync, builtins, report)
                    })
                    .await;
                sync = returned_sync;
                builtins = returned_builtins;
                if this
                    .update(cx, |this, cx| {
                        this.model.proma_sync_running = false;
                        let changed =
                            report.updated > 0 || report.catalog_changed
                                || this.model.proma_sync_warnings != report.warnings;
                        this.model.proma_sync_warnings = report.warnings;
                        if changed {
                            this.refresh(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }
}
