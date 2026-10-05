//! Typed local settings changes; no arbitrary state-file writes.
use super::*;
use crate::control::{Command, PreferencesEdit as Edit, PreferencesQuery as Query, Reply, Request};
use serde_json::{json, Value};

fn settings(state: &AppState) -> Value {
    json!([
        {"key":"toolbarSize","value":state.top_bar_size,"default":"large","type":"enum","choices":["small","medium","large"],"writable":true,"restartRequired":false},
        {"key":"sleepLockOnAc","value":state.sleep_lock_on_ac,"default":false,"type":"boolean","writable":cfg!(target_os="linux"),"restartRequired":false},
        {"key":"workspaceDefault","value":state.workspace_dir,"effective":crate::state::effective_workspace_dir(state),"default":Value::Null,"type":"absolute-directory-or-null","writable":true,"restartRequired":false},
        {"key":"settingsPanelPosition","value":state.settings_panel_pos,"default":Value::Null,"type":"integer-pair-or-null","writable":true,"restartRequired":false},
        {"key":"settingsPanelSize","value":state.settings_panel_size,"default":Value::Null,"type":"integer-pair-or-null","minimum":[660,620],"maximum":[8192,8192],"writable":true,"restartRequired":false},
        {"key":"toggleShortcut","value":state.toggle_shortcut,"type":"shortcut","writable":false,"editEntry":"Settings → Keyboard shortcut"},
        {"key":"visibleHarnesses","value":state.visible_harnesses,"default":Value::Null,"type":"harness-ids-or-null","writable":false,"editEntry":"harness visibility set/reset"}
    ])
}

impl SuperDesktopWindow {
    pub(super) fn cli_preferences(
        &self,
        model: &crate::workspace_model::LocalWorkspace,
        request: &Request,
    ) -> Option<Reply> {
        if !matches!(
            request.command,
            Command::Preferences { .. } | Command::PreferencesEdit { .. }
        ) {
            return None;
        }
        Some(self.cli_preferences_inner(model, request))
    }

    fn cli_preferences_inner(
        &self,
        model: &crate::workspace_model::LocalWorkspace,
        request: &Request,
    ) -> Reply {
        let fail = |code, message| Reply::failure(&request.request_id, code, message);
        let snapshot = match self.desktop_snapshot(model) {
            Ok(s) => s,
            Err(_) => return fail("unavailable", "Local workspace unavailable."),
        };
        let state = self.cli_live_state();
        let envelope = |state: &AppState| json!({"epoch":snapshot.epoch,"revision":crate::control_workspace::revision(state,&snapshot),"revisionScope":"workspace"});
        if let Command::Preferences { query } = &request.command {
            let mut data = envelope(&state);
            match query {
                Query::Settings { key } => {
                    let all = settings(&state);
                    if let Some(key) = key {
                        let Some(setting) =
                            all.as_array().unwrap().iter().find(|s| s["key"] == *key)
                        else {
                            return fail("not_found", "Unknown setting key.");
                        };
                        data["setting"] = setting.clone();
                    } else {
                        data["settings"] = all;
                    }
                }
                Query::HarnessArgs { id } => {
                    if !crate::tmux::HARNESS_KEYS.contains(&id.as_str()) {
                        return fail(
                            "not_found",
                            "Use a built-in harness ID; custom launchers have their own arguments.",
                        );
                    }
                    data["id"] = json!(id);
                    data["arguments"] = json!(state
                        .harness_args
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| crate::launch_args::builtin(id)));
                    data["configured"] = json!(state.harness_args.contains_key(id));
                }
                Query::Custom { id } => {
                    let Some(h) = state.custom_harnesses.iter().find(|h| h.id == *id) else {
                        return fail("not_found", "Unknown custom harness ID.");
                    };
                    data["launcher"] = json!(h);
                }
                Query::Theme => {
                    let t = crate::theme::current_theme();
                    data["theme"] = json!({"name":t.name,"mode":t.mode,"foreground":t.foreground,"background":t.background,"accent":t.accent});
                }
                Query::Usage => data["providers"] = json!(crate::usage::launcher_usage()),
            }
            return Reply::success(&request.request_id, data);
        }
        if let Err(reply) =
            crate::control_workspace::check(request, &state, &snapshot, &snapshot.epoch)
        {
            return reply;
        }
        if self.overlay_panels.iter().any(|p| p.is_visible()) {
            return fail(
                "conflict",
                "Close Settings before changing its values through the CLI.",
            );
        }
        let Command::PreferencesEdit { edit, .. } = &request.command else {
            unreachable!()
        };
        let mut next = state.clone();
        let mut result = json!({});
        let mut refresh_harnesses = false;
        match edit {
            Edit::Setting { key, value } => {
                let null = Value::Null;
                let v = value.as_ref().unwrap_or(&null);
                match key.as_str() {
                    "toolbarSize" => {
                        next.top_bar_size = if v.is_null() {
                            TopBarSize::Large
                        } else {
                            match serde_json::from_value(v.clone()) {
                                Ok(v) => v,
                                Err(_) => {
                                    return fail(
                                        "invalid_arguments",
                                        "toolbarSize is small, medium or large.",
                                    )
                                }
                            }
                        };
                    }
                    "sleepLockOnAc" => {
                        if !cfg!(target_os = "linux") {
                            return fail(
                                "unsupported_platform",
                                "Stay-awake is unsupported on this platform.",
                            );
                        }
                        next.sleep_lock_on_ac = if v.is_null() {
                            false
                        } else {
                            match v.as_bool() {
                                Some(v) => v,
                                None => {
                                    return fail(
                                        "invalid_arguments",
                                        "sleepLockOnAc requires a JSON boolean.",
                                    )
                                }
                            }
                        };
                    }
                    "workspaceDefault" => {
                        next.workspace_dir = if v.is_null() {
                            None
                        } else {
                            let Some(path) = v.as_str().filter(|p| {
                                p.len() <= 4096
                                    && std::path::Path::new(p).is_absolute()
                                    && !p.chars().any(char::is_control)
                            }) else {
                                return fail(
                                    "invalid_arguments",
                                    "workspaceDefault requires an absolute directory or null.",
                                );
                            };
                            match crate::state::clean_dir(path) {
                                Some(v) => Some(v),
                                None => return fail("invalid_arguments", "Folder does not exist."),
                            }
                        };
                    }
                    "settingsPanelPosition" | "settingsPanelSize" => {
                        let pair =
                            if v.is_null() {
                                None
                            } else {
                                match serde_json::from_value::<(i32, i32)>(v.clone()) {
                                    Ok(pair) => Some(pair),
                                    Err(_) => return fail(
                                        "invalid_arguments",
                                        "Use a JSON integer pair [x,y] or [width,height], or null.",
                                    ),
                                }
                            };
                        if key == "settingsPanelSize" {
                            if pair.is_some_and(|(w, h)| {
                                !(660..=8192).contains(&w) || !(620..=8192).contains(&h)
                            }) {
                                return fail(
                                    "invalid_arguments",
                                    "Panel size must be 660..8192 by 620..8192 logical pixels.",
                                );
                            }
                            next.settings_panel_size = pair;
                        } else {
                            if pair.is_some_and(|(x, y)| {
                                x.abs_diff(0) > 32768 || y.abs_diff(0) > 32768
                            }) {
                                return fail(
                                    "invalid_arguments",
                                    "Panel position must be within -32768..32768.",
                                );
                            }
                            next.settings_panel_pos = pair;
                        }
                    }
                    _ => {
                        return fail(
                            "invalid_arguments",
                            "Setting is unknown or read-only; see settings list.",
                        )
                    }
                }
                result["key"] = json!(key);
            }
            Edit::HarnessArgs { id, arguments } => {
                if !crate::tmux::HARNESS_KEYS.contains(&id.as_str()) {
                    return fail("not_found", "Unknown built-in harness ID.");
                }
                if let Some(arguments) = arguments {
                    if crate::launch_args::validate(arguments).is_err() {
                        return fail(
                            "invalid_arguments",
                            "Use at most 32 arguments, each at most 1024 bytes without controls.",
                        );
                    }
                    crate::launch_args::store(&mut next.harness_args, id, arguments.clone());
                } else {
                    next.harness_args.remove(id);
                }
                result["id"] = json!(id);
                result["argumentsRedacted"] = json!(true);
                refresh_harnesses = true;
            }
            Edit::CustomPut { launcher, create } => {
                let custom: crate::custom_harness::CustomHarness =
                    serde_json::from_value(json!(launcher)).unwrap();
                if custom.validate().is_err() {
                    return fail("invalid_arguments","Invalid custom launcher: use custom-ID, name up to 48 characters, supported icon, absolute executable and bounded arguments.");
                }
                let matches = next
                    .custom_harnesses
                    .iter()
                    .filter(|h| h.id == custom.id)
                    .count();
                if (*create && matches != 0) || (!create && matches != 1) {
                    return fail("conflict","Custom launcher already exists for add, or is missing/ambiguous for update.");
                }
                if *create && next.custom_harnesses.len() >= 64 {
                    return fail(
                        "limit_exceeded",
                        "At most 64 custom launchers can be configured through the CLI.",
                    );
                }
                result["id"] = json!(custom.id);
                next.custom_harnesses.retain(|h| h.id != custom.id);
                next.custom_harnesses.push(custom);
                refresh_harnesses = true;
            }
            Edit::CustomRemove { id } => {
                if !next.custom_harnesses.iter().any(|h| h.id == *id) {
                    return fail("not_found", "Unknown custom launcher ID.");
                }
                next.custom_harnesses.retain(|h| h.id != *id);
                if let Some(keys) = &mut next.visible_harnesses {
                    keys.retain(|k| k != id);
                }
                result["id"] = json!(id);
                refresh_harnesses = true;
            }
            Edit::Visibility { keys } => {
                if let Some(keys) = keys {
                    let mut seen = std::collections::HashSet::new();
                    if keys.len() > 128
                        || keys.iter().any(|k| {
                            !seen.insert(k)
                                || (!crate::tmux::HARNESS_KEYS.contains(&k.as_str())
                                    && !next.custom_harnesses.iter().any(|h| h.id == *k))
                        })
                    {
                        return fail(
                            "invalid_arguments",
                            "Use unique known harness IDs, at most 128.",
                        );
                    }
                }
                next.visible_harnesses = keys.clone();
                refresh_harnesses = true;
            }
            Edit::Rescan => refresh_harnesses = true,
            Edit::ThemeReload => {}
        }
        // Do not persist another note's still-debouncing editor text as an
        // incidental settings change; only typed setting fields differ here.
        next.notes = self.state.borrow().notes.clone();
        *self.state.borrow_mut() = next.clone();
        crate::launch_args::install(&next.harness_args);
        if let Edit::Setting { key, .. } = edit {
            match key.as_str() {
                "toolbarSize" => {
                    paint_top_bar_size(&self.hud, next.top_bar_size);
                    self.machine_view.paint_top_bar_size(next.top_bar_size);
                }
                "sleepLockOnAc" => crate::sleep_lock::set_enabled(next.sleep_lock_on_ac),
                "workspaceDefault" => {
                    let directory = crate::state::effective_workspace_dir(&next);
                    self.ws_bar.show_folder(&directory);
                }
                "settingsPanelPosition" | "settingsPanelSize" => {
                    if let Some(panel) = self.settings_layout.borrow().as_ref() {
                        panel.apply_preference(
                            next.settings_panel_pos,
                            next.settings_panel_size
                                .unwrap_or(crate::harness_settings::SETTINGS_PANEL_DEFAULT_SIZE),
                        );
                    }
                }
                _ => {}
            }
        }
        if refresh_harnesses {
            let detected = crate::tmux::detect_harnesses();
            if let Some(bar) = self.cli_harness_bar.borrow().as_ref() {
                bar.apply(&crate::harness_bar::HarnessState {
                    keys: crate::harness_settings::visible_keys(&next, &detected),
                    custom: next.custom_harnesses.iter().map(Into::into).collect(),
                    ready: true,
                });
            }
        }
        if matches!(edit, Edit::ThemeReload) {
            self.reload_theme();
        } else {
            (self.settings_refresh)();
        }
        crate::state::save_state_async(self.state.borrow().clone());
        let after = match self.desktop_snapshot(model) {
            Ok(v) => v,
            Err(_) => return Reply::unknown(&request.request_id),
        };
        Reply::success(
            &request.request_id,
            json!({"epoch":after.epoch,"revision":crate::control_workspace::revision(&self.cli_live_state(),&after),"revisionScope":"workspace","outcome":"applied","result":result,"runningSessionsChanged":false}),
        )
    }
}
