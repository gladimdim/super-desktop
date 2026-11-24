//! Local desktop adapter bridging Telegram commands to SUPER DESKTOP cards and harnesses.
use crate::prompt_attachments::{Attachment, Kind};
use serde_json::json;
use std::collections::HashMap;
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardSummary {
    pub id: String,
    pub agent_type: String,
    pub agent_name: String,
    pub icon: String,
    pub status: String,
    pub label: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Card title/prompt from native metadata or tracked input — never from raw terminal output.
    pub title: Option<String>,
    pub last_prompt: Option<String>,
    pub directory: String,
    pub directory_display: String,
    pub tag: u8,
    pub is_shell: bool,
    pub completion_supported: bool,
    pub completion_state: String,
    pub completion_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceInfo {
    pub current: String,
    pub recent: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessChoice {
    pub id: String,
    pub name: String,
    pub icon: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedKey {
    Yes,
    No,
    Digit1,
    Digit2,
    Digit3,
    Up,
    Down,
    Tab,
    ShiftTab,
    Enter,
    Escape,
    CtrlC,
}

impl NamedKey {
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "y" => Some(Self::Yes),
            "n" => Some(Self::No),
            "1" => Some(Self::Digit1),
            "2" => Some(Self::Digit2),
            "3" => Some(Self::Digit3),
            "u" => Some(Self::Up),
            "d" => Some(Self::Down),
            "t" => Some(Self::Tab),
            "b" => Some(Self::ShiftTab),
            "e" => Some(Self::Enter),
            "x" => Some(Self::Escape),
            "c" => Some(Self::CtrlC),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Yes => "Yes",
            Self::No => "No",
            Self::Digit1 => "1",
            Self::Digit2 => "2",
            Self::Digit3 => "3",
            Self::Up => "↑",
            Self::Down => "↓",
            Self::Tab => "Tab",
            Self::ShiftTab => "Shift+Tab",
            Self::Enter => "Enter",
            Self::Escape => "Escape",
            Self::CtrlC => "Ctrl-C",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardAsset {
    pub id: String,
    pub name: String,
    pub relative_path: String,
    pub mime_type: String,
    pub kind: String,
    pub size: u64,
}

impl CardAsset {
    fn from_asset(a: crate::assets::Asset) -> Self {
        Self {
            id: a.id,
            name: a.name,
            relative_path: a.relative_path,
            mime_type: a.mime_type,
            kind: a.kind,
            size: a.size,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CardReferences {
    pub assets: Vec<CardAsset>,
    pub links: Vec<String>,
}

/// Extract bounded unique HTTP(S) links from terminal output without fetching them.
pub fn extract_terminal_links(text: &str) -> Vec<String> {
    let mut offset = text.len().saturating_sub(512 * 1024);
    while !text.is_char_boundary(offset) {
        offset += 1;
    }
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for token in text[offset..]
        .split(|c: char| c.is_whitespace() || c.is_control() || "<>\"'`".contains(c))
    {
        let Some(candidate) = crate::terminal_links::clean_link(token) else {
            continue;
        };
        if seen.insert(candidate.clone()) {
            result.push(candidate);
        }
        if result.len() == 20 {
            break;
        }
    }
    result
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderUsageLimit {
    pub label: String,
    /// Fraction consumed, `0.0..=1.0` (`0.0` = fresh, `1.0` = exhausted).
    pub percent_used: f64,
    pub resets_at: String,
    pub used: Option<u64>,
    pub allowance: Option<u64>,
}

impl ProviderUsageLimit {
    pub fn is_weekly(&self) -> bool {
        let lower = self.label.to_ascii_lowercase();
        lower.contains("week")
            || lower.contains("7-day")
            || lower.contains("seven")
            || lower.contains("month")
            || lower.contains("30-day")
    }

    pub fn used_percent_int(&self) -> i64 {
        (self.percent_used.clamp(0.0, 1.0) * 100.0).round() as i64
    }

    pub fn left_percent_int(&self) -> i64 {
        ((1.0 - self.percent_used.clamp(0.0, 1.0)) * 100.0).round() as i64
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderUsageSummary {
    pub id: String,
    pub name: String,
    pub tier: String,
    pub ready: bool,
    pub limits: Vec<ProviderUsageLimit>,
    pub today_tokens: u64,
    pub today_prompts: u64,
    pub today_sessions: u64,
    pub total_prompts: u64,
    pub total_sessions: u64,
    pub status_text: String,
    pub updated_at: String,
}

pub trait DesktopBackend {
    fn list_cards(&self) -> Vec<CardSummary>;
    fn usage_summary(&self) -> Vec<ProviderUsageSummary>;
    fn final_assistant_message(&self, _card_id: &str) -> Option<String> {
        None
    }
    fn capture_screen(&self, card_id: &str, history_lines: u32) -> Result<String, String>;
    fn capture_ansi_screen(&self, card_id: &str, history_lines: u32) -> Result<String, String>;
    fn list_references(&self, card_id: &str) -> Result<CardReferences, String>;
    fn read_asset(&self, card_id: &str, asset_id: &str) -> Result<(CardAsset, Vec<u8>), String>;
    fn render_pdf_page(
        &self,
        card_id: &str,
        asset_id: &str,
        page: u32,
    ) -> Result<(CardAsset, Vec<u8>), String>;
    fn send_prompt(&self, card_id: &str, text: &str) -> Result<(), String>;
    fn send_key(&self, card_id: &str, key: NamedKey) -> Result<(), String>;
    fn send_attachment(
        &self,
        card_id: &str,
        owner: &str,
        caption: &str,
        attachment: Attachment,
    ) -> Result<(), String>;
    fn workspace_choices(&self) -> Result<WorkspaceInfo, String>;
    fn available_harnesses(&self) -> Vec<HarnessChoice>;
    fn launch_card(&self, agent_type: &str, workspace: &str) -> Result<String, String>;
    fn close_card(&self, card_id: &str) -> Result<(), String>;
}

pub struct LiveDesktopBackend;

impl LiveDesktopBackend {
    fn verify_owned_session(card_id: &str) -> Result<(), String> {
        if !card_id.starts_with("sd_term_") || !crate::tmux::session_exists(card_id) {
            return Err("Card is no longer running.".into());
        }
        Ok(())
    }

    fn session_agent(card_id: &str) -> String {
        crate::state::load_state()
            .terminals
            .into_iter()
            .find(|t| t.session_name == card_id)
            .map(|t| t.agent_type)
            .unwrap_or_else(|| "shell".to_string())
    }

    fn capture_pane(
        card_id: &str,
        history_lines: u32,
        ansi: bool,
        max_bytes: usize,
    ) -> Result<String, String> {
        Self::verify_owned_session(card_id)?;
        let target = format!("={card_id}:");
        let mut cmd = Command::new(crate::tmux::tmux_bin());
        cmd.args(["capture-pane", "-p"]);
        if ansi {
            cmd.arg("-e");
        }
        cmd.args(["-t", &target]);
        if history_lines > 0 {
            let clamped = history_lines.min(300);
            cmd.args(["-S", &format!("-{clamped}")]);
        }
        let out = cmd.output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err("Could not capture terminal screen.".into());
        }
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        if text.len() > max_bytes {
            let mut cut = max_bytes;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
        }
        Ok(text)
    }
}

impl DesktopBackend for LiveDesktopBackend {
    fn list_cards(&self) -> Vec<CardSummary> {
        let items = crate::bridge::collect_harnesses();
        let ids: Vec<String> = items
            .iter()
            .filter_map(|v| v["id"].as_str().map(str::to_string))
            .collect();
        let completions: HashMap<String, crate::completion::Completion> = if ids.is_empty() {
            HashMap::new()
        } else {
            crate::completion::collect_watched(&ids)
                .0
                .into_iter()
                .map(|c| (c.id.clone(), c))
                .collect()
        };

        items
            .into_iter()
            .filter_map(|item| {
                let id = item["id"].as_str()?.to_string();
                let agent_type = item["agentType"].as_str().unwrap_or("shell").to_string();
                let agent_name = item["agentName"].as_str().unwrap_or(&agent_type).to_string();
                let icon = item["icon"].as_str().unwrap_or("💻").to_string();
                let status = item["status"].as_str().unwrap_or("IDLE").to_string();
                let label = item["label"].as_str().unwrap_or(&status).to_string();
                let model = item["model"].as_str().map(str::to_string);
                let effort = item["effort"].as_str().map(str::to_string);
                let session_title = item["sessionTitle"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let last_prompt = item["lastPrompt"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let title = session_title.or_else(|| last_prompt.clone());
                let directory = item["directory"].as_str().unwrap_or("").to_string();
                let directory_display = item["directoryDisplay"]
                    .as_str()
                    .unwrap_or(&directory)
                    .to_string();
                let tag = item["tag"].as_u64().unwrap_or(0) as u8;
                let is_shell = crate::shell_title::is_regular(&agent_type);
                let comp = completions.get(&id);
                Some(CardSummary {
                    id,
                    agent_type,
                    agent_name,
                    icon,
                    status,
                    label,
                    model,
                    effort,
                    title,
                    last_prompt,
                    directory,
                    directory_display,
                    tag,
                    is_shell,
                    completion_supported: comp.is_some_and(|c| c.supported),
                    completion_state: comp
                        .map(|c| c.state.clone())
                        .unwrap_or_else(|| "unknown".to_string()),
                    completion_id: comp.and_then(|c| c.completion_id.clone()),
                })
            })
            .collect()
    }

    fn final_assistant_message(&self, card_id: &str) -> Option<String> {
        if Self::verify_owned_session(card_id).is_err() {
            return None;
        }
        let agent = Self::session_agent(card_id);
        match agent.as_str() {
            "codex" => crate::completion::pane_pid(card_id)
                .and_then(crate::completion::last_agent_message_for_pid),
            "claude" => {
                let meta = crate::harness_metadata::inspect(card_id, "claude")?;
                if meta.transcript.is_empty() || meta.native_session.is_empty() {
                    return None;
                }
                crate::harness_record::claude_last_assistant_message(
                    std::path::Path::new(&meta.transcript),
                    &meta.native_session,
                    256 * 1024,
                )
            }
            "pi" => {
                let meta = crate::harness_metadata::inspect(card_id, "pi")?;
                if meta.transcript.is_empty() {
                    return None;
                }
                crate::harness_record::pi_last_assistant_message(
                    std::path::Path::new(&meta.transcript),
                    256 * 1024,
                )
            }
            _ => None,
        }
    }

    fn capture_screen(&self, card_id: &str, history_lines: u32) -> Result<String, String> {
        Self::capture_pane(card_id, history_lines, false, 65_536)
    }

    fn capture_ansi_screen(&self, card_id: &str, history_lines: u32) -> Result<String, String> {
        Self::capture_pane(card_id, history_lines, true, 131_072)
    }

    fn list_references(&self, card_id: &str) -> Result<CardReferences, String> {
        Self::verify_owned_session(card_id)?;
        let _permit = crate::assets::Transfer::acquire()
            .ok_or_else(|| "Too many concurrent file transfers; try again in a moment.".to_string())?;
        let assets = crate::assets::list(card_id, None)
            .unwrap_or_default()
            .into_iter()
            .map(CardAsset::from_asset)
            .collect();
        let screen = crate::tmux::capture_file_references(card_id).unwrap_or_default();
        let links = extract_terminal_links(&screen);
        Ok(CardReferences { assets, links })
    }

    fn read_asset(&self, card_id: &str, asset_id: &str) -> Result<(CardAsset, Vec<u8>), String> {
        Self::verify_owned_session(card_id)?;
        let _permit = crate::assets::Transfer::acquire()
            .ok_or_else(|| "Too many concurrent file transfers; try again in a moment.".to_string())?;
        let (asset, bytes) =
            crate::assets::read(card_id, asset_id).map_err(|e| e.replace('_', " "))?;
        Ok((CardAsset::from_asset(asset), bytes))
    }

    fn render_pdf_page(
        &self,
        card_id: &str,
        asset_id: &str,
        page: u32,
    ) -> Result<(CardAsset, Vec<u8>), String> {
        Self::verify_owned_session(card_id)?;
        let _permit = crate::assets::Transfer::acquire()
            .ok_or_else(|| "Too many concurrent file transfers; try again in a moment.".to_string())?;
        let (asset, bytes) =
            crate::assets::read(card_id, asset_id).map_err(|e| e.replace('_', " "))?;
        if asset.kind != "pdf" {
            return Err("Selected file is not a PDF.".into());
        }
        let png = crate::asset_pdf::page(bytes, page).map_err(|e| e.replace('_', " "))?;
        Ok((CardAsset::from_asset(asset), png))
    }

    fn usage_summary(&self) -> Vec<ProviderUsageSummary> {
        crate::usage::all_provider_usage()
            .into_iter()
            .map(|u| ProviderUsageSummary {
                id: u.id,
                name: u.name,
                tier: u.tier,
                ready: u.ready,
                limits: u
                    .limits
                    .into_iter()
                    .map(|l| ProviderUsageLimit {
                        label: l.label,
                        percent_used: l.percent_used,
                        resets_at: l.resets_at,
                        used: l.used,
                        allowance: l.allowance,
                    })
                    .collect(),
                today_tokens: u.today_tokens,
                today_prompts: u.today_prompts,
                today_sessions: u.today_sessions,
                total_prompts: u.total_prompts,
                total_sessions: u.total_sessions,
                status_text: u.status_text,
                updated_at: u.updated_at,
            })
            .collect()
    }

    fn send_prompt(&self, card_id: &str, text: &str) -> Result<(), String> {
        Self::verify_owned_session(card_id)?;
        if text.is_empty() || text.len() > 4096 {
            return Err("Prompt must be 1–4096 UTF-8 bytes.".into());
        }
        if text.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
            return Err("Prompt contains unsupported control characters.".into());
        }
        let agent = Self::session_agent(card_id);
        let _guard = crate::prompt_image::input_guard(card_id)?;
        crate::tmux::send_keys(card_id, &agent, text, true)
    }

    fn send_key(&self, card_id: &str, key: NamedKey) -> Result<(), String> {
        Self::verify_owned_session(card_id)?;
        let agent = Self::session_agent(card_id);
        let _guard = crate::prompt_image::input_guard(card_id)?;
        match key {
            NamedKey::Yes => crate::tmux::send_keys(card_id, &agent, "y", true),
            NamedKey::No => crate::tmux::send_keys(card_id, &agent, "n", true),
            NamedKey::Digit1 => crate::tmux::send_keys(card_id, &agent, "1", true),
            NamedKey::Digit2 => crate::tmux::send_keys(card_id, &agent, "2", true),
            NamedKey::Digit3 => crate::tmux::send_keys(card_id, &agent, "3", true),
            NamedKey::Enter => crate::tmux::send_keys(card_id, &agent, "", true),
            NamedKey::Escape | NamedKey::CtrlC | NamedKey::Up | NamedKey::Down | NamedKey::Tab | NamedKey::ShiftTab => {
                let tmux_key = match key {
                    NamedKey::Escape => "Escape",
                    NamedKey::CtrlC => "C-c",
                    NamedKey::Up => "Up",
                    NamedKey::Down => "Down",
                    NamedKey::Tab => "Tab",
                    NamedKey::ShiftTab => "BTab",
                    _ => unreachable!(),
                };
                let target = format!("={card_id}:");
                let out = Command::new(crate::tmux::tmux_bin())
                    .args(["send-keys", "-t", &target, tmux_key])
                    .output()
                    .map_err(|e| e.to_string())?;
                if out.status.success() {
                    Ok(())
                } else {
                    Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
                }
            }
        }
    }

    fn send_attachment(
        &self,
        card_id: &str,
        owner: &str,
        caption: &str,
        attachment: Attachment,
    ) -> Result<(), String> {
        Self::verify_owned_session(card_id)?;
        if caption.len() > 4096 || caption.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err("Caption contains invalid control characters or exceeds 4096 bytes.".into());
        }
        let request_id = super_desktop::control::random_hex(16).map_err(|e| e.to_string())?;
        crate::prompt_attachments::submit(
            card_id,
            owner,
            caption,
            &request_id,
            &[attachment],
            || true,
        )
    }

    fn workspace_choices(&self) -> Result<WorkspaceInfo, String> {
        if let crate::Ipc::Reply(reply) = crate::ipc_request("workspace-choices") {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&reply) {
                if let Some(current) = val["workspace"].as_str().map(str::to_string) {
                    let recent = val["recentDirectories"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    return Ok(WorkspaceInfo { current, recent });
                }
            }
        }
        let state = crate::state::load_state();
        Ok(WorkspaceInfo {
            current: crate::state::effective_workspace_dir(&state),
            recent: crate::state::shared_recent_dirs(&state).to_vec(),
        })
    }

    fn available_harnesses(&self) -> Vec<HarnessChoice> {
        let state = crate::state::load_state();
        let mut choices: Vec<HarnessChoice> = crate::tmux::HARNESS_KEYS
            .iter()
            .filter(|key| crate::tmux::detect_harness_command(key).is_some())
            .map(|key| {
                let cfg = crate::tmux::get_agent_config(key);
                HarnessChoice {
                    id: (*key).to_string(),
                    name: cfg.name.to_string(),
                    icon: cfg.icon.to_string(),
                }
            })
            .collect();
        for item in state.custom_harnesses {
            if item.available() {
                choices.push(HarnessChoice {
                    id: item.id,
                    name: item.name,
                    icon: item.icon,
                });
            }
        }
        choices
    }

    fn launch_card(&self, agent_type: &str, workspace: &str) -> Result<String, String> {
        let available = self.available_harnesses();
        if !available.iter().any(|h| h.id == agent_type) {
            return Err("Selected harness is not available on this PC.".into());
        }
        let choices = self.workspace_choices()?;
        let allowed_dir = choices.current == workspace
            || choices.recent.iter().any(|d| d == workspace);
        let cleaned = crate::state::clean_dir(workspace)
            .filter(|_| allowed_dir)
            .ok_or_else(|| "Workspace folder is not in the PC's offered folder list.".to_string())?;
        let cmd = format!(
            "add-term-in {}",
            json!({ "agentType": agent_type, "workspace": cleaned })
        );
        match crate::ipc_request(&cmd) {
            crate::Ipc::Reply(reply) => {
                let val: serde_json::Value =
                    serde_json::from_str(&reply).unwrap_or(serde_json::Value::Null);
                if val["ok"] == true {
                    val["id"]
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| "Daemon launched card without an ID.".to_string())
                } else {
                    Err(val["error"]
                        .as_str()
                        .unwrap_or("launch_failed")
                        .to_string())
                }
            }
            crate::Ipc::NoDaemon => Err("SUPER DESKTOP daemon is not running.".into()),
            crate::Ipc::Stalled => Err("SUPER DESKTOP daemon timed out launching card.".into()),
        }
    }

    fn close_card(&self, card_id: &str) -> Result<(), String> {
        if !card_id.starts_with("sd_term_") {
            return Err("Invalid card ID.".into());
        }
        match crate::ipc_request(&format!("close-term {card_id}")) {
            crate::Ipc::Reply(reply) => {
                let val: serde_json::Value =
                    serde_json::from_str(&reply).unwrap_or(serde_json::Value::Null);
                if val["ok"] == true {
                    Ok(())
                } else {
                    Err(val["error"]
                        .as_str()
                        .unwrap_or("close_failed")
                        .to_string())
                }
            }
            crate::Ipc::NoDaemon => Err("SUPER DESKTOP daemon is not running.".into()),
            crate::Ipc::Stalled => Err("SUPER DESKTOP daemon timed out closing card.".into()),
        }
    }
}

pub fn normalize_telegram_image(bytes: &[u8]) -> Result<Attachment, String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let encoded = STANDARD.encode(bytes);
    let png = crate::prompt_image::normalize(&encoded)?;
    Ok(Attachment {
        kind: Kind::Image,
        name: "photo.png".to_string(),
        bytes: png,
    })
}
