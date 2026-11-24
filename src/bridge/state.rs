use super::*;

// ---------- session truth (reuses crate::tmux) ----------

pub(crate) fn last_user_text(
    session: &str,
    agent_type: &str,
    persisted: Option<&str>,
) -> Option<String> {
    if crate::shell_title::is_regular(agent_type) {
        return crate::shell_title::last(session);
    }
    // A silent native adapter falls back to the typed prompt (same rule as
    // the card, see `reported_or_typed_prompt`).
    let metadata = crate::harness_metadata::inspect(session, agent_type);
    if let Some(prompt) = crate::card_status::reported_or_typed_prompt(
        metadata.as_ref(),
        || crate::prompt_history::last(session),
    ) {
        if prompt.is_some() {
            return prompt;
        }
        if let Some(m) = metadata.filter(|m| m.adapter_reported() && !m.native_session.is_empty()) {
            match agent_type {
                "antigravity" | "agy" => {
                    if let Some(p) = crate::antigravity::latest_prompt(&m.native_session) {
                        return Some(crate::tmux::truncate_prompt_title(&p));
                    }
                    return crate::prompt_history::last(session)
                        .filter(|p| crate::harness_record::is_user_prompt(p));
                }
                "hermes" => {
                    if let Some(p) = crate::hermes::latest_prompt(&m.native_session) {
                        return Some(crate::tmux::truncate_prompt_title(&p));
                    }
                    return crate::prompt_history::last(session)
                        .filter(|p| crate::harness_record::is_user_prompt(p));
                }
                "pi" if !m.transcript.is_empty() => {
                    if let Some((recs, _)) = crate::harness_record::pi_prompts(
                        std::path::Path::new(&m.transcript),
                        64 * 1024,
                    ) {
                        if let Some(last) = recs.into_iter().last() {
                            return Some(crate::tmux::truncate_prompt_title(&last.text));
                        }
                    }
                }
                _ => {}
            }
        }
        return None;
    }
    if agent_type == "codex" {
        return crate::completion::last_user_prompt(session);
    }
    if agent_type == "grok" {
        if let Some(pid) = crate::completion::pane_pid(session) {
            if let Some(p) = crate::grok_session::latest_prompt_for_pid(pid, None) {
                return Some(crate::tmux::truncate_prompt_title(&p));
            }
        }
    }
    // Exact opencode DB text is resolved to the session OWNED by this pane
    // (own `--session` flag, else a claims-aware match), so a closed console's
    // prompt never leaks here. Composer drafts stay in their separate field.
    if agent_type == "opencode" {
        if let Some(id) = resolve_own_opencode_id(session, persisted) {
            if let Some(text) = get_opencode_user_text_by_id(&id) {
                return Some(text);
            }
        }
    }
    // Response text is never a fallback for AI harness titles.
    None
}

pub(crate) fn session_title(session: &str, agent_type: &str, pane_pid: &str) -> Option<String> {
    if let Some(title) = crate::harness_metadata::title(session, agent_type) {
        return Some(title);
    }
    match agent_type {
        "codex" => crate::completion::session_title(pane_pid.parse().ok()?),
        "grok" => crate::grok_session::title_for_pid(pane_pid.parse().ok()?, None),
        "antigravity" | "agy" => crate::harness_metadata::inspect(session, agent_type)
            .filter(|m| !m.native_session.is_empty())
            .and_then(|m| crate::antigravity::summary_title(&m.native_session)),
        "hermes" => crate::harness_metadata::inspect(session, agent_type)
            .filter(|m| !m.native_session.is_empty())
            .and_then(|m| crate::hermes::session_title(&m.native_session)),
        _ => None,
    }
}

/// Hex colour of a super-desktop group tag (0 = untagged).
///
/// One source of truth: the same palette the desktop dots use (`tag::TAG_COLORS`,
/// mirrored as `.tag-dot-N` in styles.rs).
pub(in crate::bridge) fn tag_color(tag: u8) -> Option<&'static str> {
    let n = crate::tag::normalize_tag(tag);
    if n == crate::tag::TAG_NONE {
        None
    } else {
        crate::tag::TAG_COLORS.get(n as usize - 1).copied()
    }
}

pub fn collect_harnesses() -> Vec<serde_json::Value> {
    let state = load_state();
    let snapshot = crate::tmux::pane_snapshot();
    harness_list::collect(&state, snapshot.as_ref())
}

// ---------- pairing state ----------

#[derive(Debug, Serialize, Deserialize)]
pub(in crate::bridge) struct BridgeConfig {
    #[serde(default = "new_bridge_id")]
    pub(in crate::bridge) bridge_id: String,
    #[serde(default)]
    pub(in crate::bridge) paired_tokens: Vec<String>,
    #[serde(default)]
    pub(in crate::bridge) devices: Vec<PairedDevice>,
    /// Devices whose pairing request the owner rejected. Their later requests
    /// are refused without a desktop prompt until the owner removes them.
    #[serde(default)]
    pub(in crate::bridge) rejected: Vec<RejectedDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(in crate::bridge) struct PairedDevice { pub(in crate::bridge) id: String, pub(in crate::bridge) name: String, pub(in crate::bridge) token_hash: String, pub(in crate::bridge) expires: f64,
    #[serde(default)]
    pub(in crate::bridge) device_type: String,
}

/// Everything here was reported by the rejected device itself, so it is only
/// used to recognise that device again, never to grant anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(in crate::bridge) struct RejectedDevice {
    /// Local handle for removing the entry; not the device's own identity.
    pub(in crate::bridge) id: String,
    pub(in crate::bridge) name: String,
    #[serde(default)]
    pub(in crate::bridge) device_type: String,
    /// The installation ID a SUPER DESKTOP PC sends (its bridge ID). Empty for
    /// clients that do not send one.
    #[serde(default)]
    pub(in crate::bridge) device_id: String,
    /// Source address of the rejected request.
    #[serde(default)]
    pub(in crate::bridge) address: String,
    pub(in crate::bridge) rejected_at: f64,
}

pub(in crate::bridge) fn new_bridge_id() -> String { random_hex(16) }

/// Read identity without initializing or rewriting the bridge credential store.
pub fn own_bridge_id() -> Option<String> {
    let dir = std::env::var_os("SUPER_DESKTOP_BRIDGE_STATE_DIR").map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state/omarchy/harness-bridge")))?;
    let value: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("config.json")).ok()?).ok()?;
    value["bridge_id"].as_str().map(str::to_owned)
}

pub(in crate::bridge) fn state_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    // Unit tests exercise the pairing routes in-process; they must never
    // write the developer's real credential store.
    let default = if cfg!(test) {
        std::env::temp_dir().join(format!("sd-bridge-test-{}", std::process::id()))
    } else {
        PathBuf::from(home).join(".local/state/omarchy/harness-bridge")
    };
    let dir = std::env::var_os("SUPER_DESKTOP_BRIDGE_STATE_DIR").map(PathBuf::from)
        .unwrap_or(default);
    fs::create_dir_all(&dir).expect("Cannot create private bridge state directory");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("Cannot secure bridge state directory");
    dir.join("config.json")
}

pub(in crate::bridge) fn random_hex(bytes: usize) -> String {
    crate::control::random_hex(bytes)
        .expect("OS randomness unavailable; refusing to generate pairing credentials")
}

pub(in crate::bridge) struct PairState {
    pub(in crate::bridge) cfg: BridgeConfig,
    pub(in crate::bridge) mtime: f64,
}

pub(in crate::bridge) fn config_mtime() -> f64 {
    fs::metadata(state_path())
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs_f64())
        })
        .unwrap_or(0.0)
}

impl PairState {
    pub(in crate::bridge) fn load() -> Self {
        let path = state_path();
        let cfg: BridgeConfig = fs::read_to_string(&path)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_else(|| BridgeConfig {
                bridge_id: new_bridge_id(),
                paired_tokens: vec![],
                devices: vec![],
                rejected: vec![],
            });
        let mut state = Self {
            cfg,
            mtime: 0.0,
        };
        // Legacy credentials travelled over HTTP; never accept them under v3.
        state.cfg.paired_tokens.clear();
        state.save();
        state.mtime = config_mtime();
        state
    }

    pub(in crate::bridge) fn save(&self) -> bool {
        use std::os::unix::fs::OpenOptionsExt;
        let path = state_path();
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        (|| -> std::io::Result<()> {
            let json = serde_json::to_vec_pretty(&self.cfg)?;
            let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temporary)?;
            file.write_all(&json)?;
            file.sync_all()?;
            fs::rename(temporary, path)
        })().is_ok()
    }

    /// Pick up config.json edits made by another process (e.g. a fresh PIN
    /// written from the overlay settings while the bridge keeps running).
    /// Tokens minted in-memory are unioned in so nothing is lost.
    pub(in crate::bridge) fn refresh(&mut self) {
        let m = config_mtime();
        if m == 0.0 || m == self.mtime {
            return;
        }
        if let Some(disk) = fs::read_to_string(state_path())
            .ok()
            .and_then(|c| serde_json::from_str::<BridgeConfig>(&c).ok())
        {
            self.cfg = BridgeConfig {
                bridge_id: self.cfg.bridge_id.clone(),
                paired_tokens: vec![],
                devices: disk.devices,
                rejected: disk.rejected,
            };
        }
        self.mtime = config_mtime();
    }

    pub(in crate::bridge) fn valid(&mut self, token: &str) -> bool {
        self.refresh();
        if token.is_empty() {
            return false;
        }
        let hash = security::digest(token.as_bytes());
        let now = now_epoch();
        self.cfg.devices.iter().any(|d| d.expires > now && security::equal(&d.token_hash, &hash))
    }
}

pub(in crate::bridge) fn pair_state() -> &'static Mutex<PairState> {
    static CELL: OnceLock<Mutex<PairState>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(PairState::load()))
}

