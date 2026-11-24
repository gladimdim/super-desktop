use super::*;

/// Ordered input on a persistent socket. Never replay an input after a lost
/// acknowledgement: the client must treat that outcome as uncertain.
pub(in crate::bridge) fn stream_keys(stream: &mut Connection, id: &str) {
    let (agent, _) = session_meta(id);
    stream_keys_as(stream, id, &agent);
}

pub(in crate::bridge) fn stream_keys_as(stream: &mut Connection, id: &str, agent: &str) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Ok(mut control) = crate::tmux_control::Control::open(id) else {
        let _ = crate::ws::write_close(stream, 1011, "terminal unavailable");
        return;
    };
    loop {
        match crate::ws::read_frame(stream) {
            Ok(Some(crate::ws::Frame::Text(text))) => {
                let Ok(body) = serde_json::from_str::<serde_json::Value>(&text) else { break };
                let value = body["text"].as_str().unwrap_or("");
                let enter = body["enter"].as_bool().unwrap_or(false);
                let result = (|| -> Result<(), String> {
                    let _input = crate::prompt_image::input_guard(id)?;
                    if value.len() > 4096 { return Err("text_too_long".into()); }
                    if body["checkIdle"].as_bool().unwrap_or(false) {
                        if !matches!(inspect_status(id, agent).status, "IDLE" | "FINISHED") {
                            return Err("Wait for the harness to become idle.".into());
                        }
                        if get_composer_draft(id).is_some_and(|s| !s.trim().is_empty()) {
                            return Err("Finish or clear the remote draft first.".into());
                        }
                    }
                    if !stream.still_authorized() { return Err("device_revoked".into()); }
                    // One failed tmux command poisons the client for good, and
                    // this socket can live for hours. Reopen it for this input;
                    // the input that failed was acknowledged as an error and is
                    // never replayed.
                    if !control.is_healthy() {
                        control = crate::tmux_control::Control::open(id)?;
                    }
                    // A harness must finish reading the text as a paste
                    // before Return, or Return lands in the draft as a
                    // newline. The wait stays here, without another
                    // phone-to-bridge round trip.
                    let settle = enter && !value.is_empty() && crate::tmux::settles_before_enter(agent);
                    // Probe outside the control client: a failed capture
                    // there would poison it before Return is sent.
                    let before = if settle { crate::tmux::capture_visible_screen(id) } else { None };
                    control.send(value, false)?;
                    wake_terminal_streams(id);
                    if settle {
                        crate::tmux::settle_before_enter(before.as_deref(), || crate::tmux::capture_visible_screen(id));
                    }
                    if !stream.still_authorized() { return Err("device_revoked".into()); }
                    if enter { control.send("", true)?; }
                    // The persistent phone input path bypasses tmux::send_keys,
                    // which normally records submitted composer text. Record
                    // only after Enter succeeds, never an unsent draft or error.
                    if enter {
                        crate::prompt_history::record(id, value);
                    }
                    wake_terminal_streams(id);
                    Ok(())
                })();
                let ack = serde_json::json!({"sequence": body["sequence"],
                    "ok": result.is_ok(), "error": result.err()});
                if crate::ws::write_text(stream, &ack.to_string()).is_err() { break; }
            }
            Ok(Some(crate::ws::Frame::Ping(payload))) => {
                if crate::ws::write_pong(stream, &payload).is_err() { break; }
            }
            Ok(Some(crate::ws::Frame::Pong(_))) => {},
            _ => break,
        }
    }
}

/// Agent type + group tag of a session, from the desktop state file.
pub(in crate::bridge) fn session_meta(session: &str) -> (String, u8) {
    load_state()
        .terminals
        .iter()
        .find(|t| t.session_name == session)
        .map(|t| (t.agent_type.clone(), t.tag))
        .unwrap_or_else(|| ("shell".to_string(), 0))
}

/// `GET /api/v1/harnesses/<id>/prompts` — the terminal's submitted prompts,
/// newest first (`prompt_log`). Read-only; an older PC answers 404 without
/// the `no_such_session` error, which tells the phone to ask for an update.
pub(in crate::bridge) fn handle_prompts(stream: &mut Connection, req: &Request, id: &str) {
    if !require_live_session(stream, req, id, Envelope::Status) {
        return;
    }
    let (agent, persisted) = load_state()
        .terminals
        .iter()
        .find(|t| t.session_name == id)
        .map(|t| (t.agent_type.clone(), t.agent_session_id.clone()))
        .unwrap_or_else(|| ("shell".to_string(), None));
    let history = crate::prompt_log::history(id, &agent, persisted.as_deref());
    if !stream.still_authorized() {
        return;
    }
    reply_json(stream, 200, &history.to_json());
}

/// `POST /api/v1/harnesses/<id>/keys` — type into a harness from the phone.
///
/// Body: `{"text": "ls -la", "enter": true}`. `text` is optional (so the phone
/// can send a bare Return, or a control byte such as `\u0003` for Ctrl-C).
pub(in crate::bridge) fn handle_keys(stream: &mut Connection, req: &Request, id: &str) {
    if !require_live_session(stream, req, id, Envelope::Status) {
        return;
    }
    let Ok(body) = serde_json::from_str::<serde_json::Value>(&req.body) else {
        return reply_error(stream, 400, Envelope::Status, "bad_json");
    };
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let enter = body.get("enter").and_then(|v| v.as_bool()).unwrap_or(true);
    if text.len() > 4096 {
        return reply_error(stream, 413, Envelope::Status, "text_too_long");
    }
    let (agent, _) = session_meta(id);
    let result = crate::prompt_image::input_guard(id).and_then(|_guard| crate::tmux::send_keys(id, &agent, text, enter));
    match result {
        Ok(()) => reply_json(stream, 200, &serde_json::json!({"status": "ok"})),
        Err(e) => reply_error(stream, 500, Envelope::Status, e),
    }
}

/// `POST /api/v1/harnesses/<id>/editor-action` — Save, Close, scroll and the
/// other actions a stream frame's `editor` lists, typed in that editor's own
/// keys. The body names the editor the phone saw; when the pane no longer runs
/// it, nothing is typed (`editor_not_foreground`), so an answer meant for the
/// editor never reaches a shell or a harness prompt.
pub(in crate::bridge) fn handle_editor_action(stream: &mut Connection, req: &Request, id: &str) {
    use crate::editor_actions::{Action, Error};
    if !require_live_session(stream, req, id, Envelope::Status) {
        return;
    }
    let Ok(body) = serde_json::from_str::<serde_json::Value>(&req.body) else {
        return reply_error(stream, 400, Envelope::Status, "bad_json");
    };
    let parsed = body
        .get("editor")
        .and_then(|v| v.as_str())
        .ok_or(Error::InvalidRequest)
        .and_then(|editor| Action::parse(&body).map(|action| (editor, action)));
    let result = parsed.and_then(|(editor, action)| {
        let _input = crate::prompt_image::input_guard(id).map_err(Error::Tmux)?;
        if !stream.still_authorized() {
            return Err(Error::Tmux("device_revoked".into()));
        }
        let mut control = crate::tmux_control::Control::open(id).map_err(Error::Tmux)?;
        crate::editor_actions::run(&mut control, editor, &action)
    });
    match result {
        Ok(editor) => {
            wake_terminal_streams(id);
            reply_json(stream, 200, &serde_json::json!({"status": "ok", "editor": editor}))
        }
        Err(Error::NotForeground(foreground)) => reply_json(
            stream,
            409,
            &serde_json::json!({"status": "error", "error": "editor_not_foreground", "foreground": foreground}),
        ),
        Err(error @ (Error::InvalidRequest | Error::InvalidFileName | Error::UnsupportedAction)) => {
            reply_error(stream, 400, Envelope::Status, error.code())
        }
        Err(Error::Tmux(error)) => {
            let code = match error.as_str() {
                "terminal_input_busy_try_again" => 409,
                "device_revoked" => 403,
                _ => 500,
            };
            reply_error(stream, code, Envelope::Status, error)
        }
    }
}

/// `DELETE /api/v1/harnesses/<id>` — close one launcher-visible harness.
pub(in crate::bridge) fn handle_close_harness(stream: &mut Connection, req: &Request, id: &str) {
    if !require_pairing(stream, &req.headers, Envelope::Status) {
        return;
    }
    if !id.starts_with("sd_term_") {
        return reply_error(stream, 404, Envelope::Status, "no_such_session");
    }
    match crate::ipc_request(&format!("close-term {id}")) {
        crate::Ipc::Reply(reply) => {
            let result: serde_json::Value =
                serde_json::from_str(&reply).unwrap_or(serde_json::Value::Null);
            if result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                reply_json(stream, 200, &result)
            } else if result.get("error").and_then(|v| v.as_str()) == Some("no_such_session") {
                reply_json(stream, 404, &result)
            } else {
                reply_error(stream, 500, Envelope::Status, "close_failed")
            }
        }
        crate::Ipc::NoDaemon => reply_error(stream, 503, Envelope::Status, "desktop_not_running"),
        crate::Ipc::Stalled => {
            reply_error(stream, 504, Envelope::Status, "close_timeout_check_machine_before_retry")
        }
    }
}

/// `POST /api/v1/harnesses/<id>/relaunch` — restart a card fresh or resume its conversation.
pub(in crate::bridge) fn handle_relaunch_harness(stream: &mut Connection, req: &Request, id: &str) {
    if !require_pairing(stream, &req.headers, Envelope::Plain) {
        return;
    }
    if !id.starts_with("sd_term_") {
        return reply_error(stream, 404, Envelope::Plain, "no_such_session");
    }
    let Ok(body) = serde_json::from_str::<serde_json::Value>(&req.body) else {
        return reply_error(stream, 400, Envelope::Plain, "bad_json");
    };
    let mode = match body.get("mode") {
        None => "fresh",
        Some(v) => match v.as_str() {
            Some("fresh") => "fresh",
            Some("resume") => "resume",
            _ => return reply_error(stream, 400, Envelope::Plain, "invalid_mode"),
        },
    };
    match crate::ipc_request(&format!("relaunch-term {id} {mode}")) {
        crate::Ipc::Reply(reply) => {
            let result: serde_json::Value =
                serde_json::from_str(&reply).unwrap_or(serde_json::Value::Null);
            if result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                return reply_json(stream, 200, &result);
            }
            match result.get("error").and_then(|v| v.as_str()) {
                Some("no_such_session") => reply_error(stream, 404, Envelope::Plain, "no_such_session"),
                Some("unsupported_resume") => reply_error(stream, 400, Envelope::Plain, "unsupported_resume"),
                Some("invalid_arguments") => reply_error(stream, 400, Envelope::Plain, "invalid_arguments"),
                Some("invalid_state") => reply_error(stream, 409, Envelope::Plain, "invalid_state"),
                Some("desktop_unavailable") => reply_error(stream, 503, Envelope::Plain, "desktop_unavailable"),
                _ => reply_error(stream, 500, Envelope::Plain, "relaunch_failed"),
            }
        }
        crate::Ipc::NoDaemon => reply_error(stream, 503, Envelope::Plain, "desktop_not_running"),
        crate::Ipc::Stalled => {
            reply_error(stream, 504, Envelope::Plain, "relaunch_timeout_check_machine_before_retry")
        }
    }
}

pub(in crate::bridge) fn creation_command(agent: &str, body: &serde_json::Value) -> Option<String> {
    if let Some(value) = body.get("workspace") {
        let directory = value.as_str().and_then(crate::state::clean_dir)?;
        Some(format!("add-term-in {}", serde_json::json!({"agentType":agent,"workspace":directory})))
    } else {
        Some(format!("add-term {agent}"))
    }
}

/// Idle time a kept-alive connection waits for its next request.
pub(in crate::bridge) const KEEP_ALIVE_IDLE: Duration = Duration::from_secs(30);
/// Requests served on one connection before it is closed.
pub(in crate::bridge) const KEEP_ALIVE_MAX_REQUESTS: usize = 200;

/// Serve one connection: a single request, or several with HTTP/1.1
/// keep-alive. Every request is parsed, checked (browser origin, pairing,
/// revocation) and answered on its own; a WebSocket upgrade, a binary asset or
/// `Connection: close` ends the loop. The owner-only Unix socket is served
/// sequentially, so it never keeps a connection open.
pub(in crate::bridge) fn serve_connection(mut stream: Connection, admission: Option<security::Admission>) {
    let mut served = 0;
    while handle_client(&mut stream, admission.as_ref()) {
        served += 1;
        if served >= KEEP_ALIVE_MAX_REQUESTS || !stream.await_next_request(KEEP_ALIVE_IDLE) {
            break;
        }
    }
}

/// Read and answer one request. Returns whether the connection may carry
/// another one.
pub(in crate::bridge) fn handle_client(stream: &mut Connection, admission: Option<&security::Admission>) -> bool {
    stream.persist = false;
    stream.forget_credential();
    let Some(req) = read_request(stream, admission) else {
        return false;
    };
    stream.persist = req.keep_alive && !stream.is_local();
    route(stream, &req, admission);
    stream.persist
}

pub(in crate::bridge) fn route(stream: &mut Connection, req: &Request, admission: Option<&security::Admission>) {
    let local = stream.is_local();
    if req.headers.contains_key("origin") || req.headers.contains_key("sec-fetch-site") {
        return reply_error(stream, 403, Envelope::Plain, "browser_access_disabled");
    }
    if let Some(guard) = admission { guard.identify(bearer(&req.headers)); }
    // Recheck on each stream I/O as well as closing the socket: TLS may already
    // have buffered input when a device is revoked.
    if authorize(&req.headers) {
        let token = bearer(&req.headers);
        security::note_activity(token);
        stream.credential(token);
    }
    let path = req.path.as_str();
    if path == "/api/v1/previews" || path.starts_with("/api/v1/previews/") {
        if !require_pairing(stream, &req.headers, Envelope::Plain) { return; }
        return previews::handle(stream, req);
    }
    if req.method == "POST" && crate::transcribe::route(path) {
        if !require_pairing(stream, &req.headers, Envelope::Plain) {
            return;
        }
        stream.streaming();
        stream.persist = false;
        let wav = match crate::transcribe::parse_request(&req.body) {
            Ok(bytes) => bytes,
            Err(error) => return reply_error(stream, 400, Envelope::Plain, error),
        };
        return match crate::transcribe::transcribe_wav(&wav) {
            Ok((text, engine)) => reply_json(
                stream,
                200,
                &serde_json::json!({"text": text, "engine": engine}),
            ),
            Err("stt_unavailable") => {
                reply_error(stream, 501, Envelope::Plain, "stt_unavailable")
            }
            Err(error) => reply_error(stream, 502, Envelope::Plain, error),
        };
    }
    if req.method == "POST" {
        let image = crate::prompt_image::route(path);
        if let Some(session) = image.or_else(|| crate::prompt_attachments::route(path)) {
            if !require_pairing(stream, &req.headers, Envelope::Plain) {
                return;
            }
            stream.streaming();
            // Uploads run under their own long deadline: never reuse the socket.
            stream.persist = false;
            let owner = security::digest(bearer(&req.headers).as_bytes());
            let mut transcribed: Option<(String, &'static str)> = None;
            let result = if image.is_some() {
                let body = serde_json::from_str(&req.body).unwrap_or_default();
                crate::prompt_image::submit(session, &owner, &body, || stream.still_authorized())
            } else {
                crate::prompt_attachments::parse_request(&req.body).and_then(|parsed| {
                    if let Some(voiced) = crate::prompt_attachments::resolve_voice_transcription_with(
                        &parsed,
                        crate::transcribe::transcribe_wav,
                    ) {
                        transcribed = Some((voiced.transcript, voiced.engine));
                        crate::prompt_attachments::submit(
                            session,
                            &owner,
                            &voiced.combined_text,
                            &parsed.request,
                            &voiced.remaining,
                            || stream.still_authorized(),
                        )
                    } else {
                        crate::prompt_attachments::submit(
                            session,
                            &owner,
                            &parsed.text,
                            &parsed.request,
                            &parsed.attachments,
                            || stream.still_authorized(),
                        )
                    }
                })
            };
            wake_terminal_streams(session);
            return match result {
                Ok(()) => {
                    let body = if let Some((transcript, engine)) = transcribed {
                        serde_json::json!({"status": "submitted", "transcript": transcript, "engine": engine})
                    } else {
                        serde_json::json!({"status": "submitted"})
                    };
                    reply_json(stream, 200, &body)
                }
                Err(error) => reply_error(stream, 409, Envelope::Plain, error),
            };
        }
    }

    // All pairing routes go through explicit desktop approval. In particular,
    // neither a legacy PIN nor an open window can mint a token any longer.
    if path == "/api/v1/pair" || path.starts_with("/api/v1/pair/") {
        return pairing::handle(stream, req, local, path);
    }

    if let Some(rest) = path.strip_prefix("/api/v1/harnesses/") {
        let parts: Vec<_> = rest.split('/').collect();
        if parts.get(1) == Some(&"assets") {
            if !require_pairing(stream, &req.headers, Envelope::Plain) {
                return;
            }
            // Authenticated asset work has its own bounded renderer timeout;
            // the initial TLS/header/body deadline no longer applies.
            stream.streaming();
            let Some(_permit) = crate::assets::Transfer::acquire() else {
                return reply_error(stream, 429, Envelope::Plain, "asset_transfer_busy");
            };
            if parts.len() == 2 && (req.method == "GET" || req.method == "POST") {
                let body: serde_json::Value = serde_json::from_str(&req.body).unwrap_or_default();
                let explicit = body["path"].as_str();
                if req.method == "POST" && explicit.is_none() {
                    return reply_error(stream, 400, Envelope::Plain, "missing_path");
                }
                return match crate::assets::list(parts[0], if req.method == "POST" { explicit } else { None }) {
                    Ok(items) => reply_json(stream, 200, &serde_json::json!({"assets":items,"maxFileBytes":crate::assets::MAX_FILE})),
                    Err(error) => reply_error(stream, 400, Envelope::Plain, error),
                };
            }
            if parts.len() == 4 && parts[3] == "content" && req.method == "GET" {
                return match crate::assets::read(parts[0], parts[2]) {
                    Ok((asset, bytes)) => reply_binary(stream, &asset.mime_type,
                        &["Content-Disposition: attachment", "X-Content-Type-Options: nosniff", "Cache-Control: no-store"], &bytes),
                    Err(error) => reply_error(stream, 404, Envelope::Plain, error),
                };
            }
            if parts.len() == 5 && parts[3] == "pages" && req.method == "GET" {
                let result = crate::assets::read(parts[0], parts[2]).and_then(|(asset, bytes)| {
                    if asset.kind != "pdf" { return Err("not_a_pdf".into()); }
                    let page = parts[4].parse().map_err(|_| "invalid_page")?;
                    crate::asset_pdf::page(bytes, page)
                });
                return match result {
                    Ok(bytes) => reply_binary(stream, "image/png", &["Cache-Control: no-store", "X-Content-Type-Options: nosniff"], &bytes),
                    Err(error) => reply_error(stream, 400, Envelope::Plain, error),
                };
            }
            return reply_error(stream, 404, Envelope::Plain, "unknown_asset_route");
        }
    }

    if path == "/api/v1/workspaces" || path == "/api/v1/harness-types" || (path == "/api/v1/harnesses" && req.method == "POST") {
        if !require_pairing(stream, &req.headers, Envelope::Plain) {
            return;
        }
        if req.method == "GET" && path == "/api/v1/workspaces" {
            return match crate::ipc_request("workspace-choices") {
                crate::Ipc::Reply(reply) => match serde_json::from_str::<serde_json::Value>(&reply) {
                    Ok(value) => reply_json(stream, 200, &value),
                    Err(_) => reply_error(stream, 502, Envelope::Plain, "invalid_desktop_response"),
                },
                _ => reply_error(stream, 503, Envelope::Plain, "desktop_not_running"),
            };
        }
        if req.method == "GET" && path == "/api/v1/harness-types" {
            let state = load_state();
            let types: Vec<_> = crate::tmux::HARNESS_KEYS.iter().map(|key| {
                let config = get_agent_config(key);
                serde_json::json!({"id":key,"name":config.name,"icon":config.icon,
                    "available":crate::tmux::detect_harness_command(key).is_some()})
            }).chain(state.custom_harnesses.iter().map(|item| {
                serde_json::json!({"id":item.id,"name":item.name,"icon":item.icon,"available":item.available()})
            })).collect();
            return reply_json(stream, 200, &serde_json::json!({"types":types,
                "workspace":crate::state::effective_workspace_dir(&state)}));
        }
        if req.method == "POST" && path == "/api/v1/harnesses" {
            let body: serde_json::Value = serde_json::from_str(&req.body).unwrap_or(serde_json::Value::Null);
            let agent = body.get("agentType").and_then(|v| v.as_str()).unwrap_or("");
            let custom = load_state().custom_harnesses.into_iter().find(|item| item.id == agent);
            if !crate::tmux::HARNESS_KEYS.contains(&agent) && custom.is_none() {
                return reply_error(stream, 400, Envelope::Plain, "unsupported_harness");
            }
            let available = custom.as_ref().map_or_else(
                || crate::tmux::detect_harness_command(agent).is_some(),
                |item| item.validate().is_ok(),
            );
            if !available {
                return reply_error(stream, 409, Envelope::Plain, "harness_not_installed");
            }
            let Some(command) = creation_command(agent, &body) else {
                return reply_error(stream, 400, Envelope::Plain, "invalid_workspace");
            };
            return match crate::ipc_request(&command) {
                crate::Ipc::Reply(reply) => {
                    let result: serde_json::Value = serde_json::from_str(&reply).unwrap_or(serde_json::Value::Null);
                    if result["ok"] == true {
                        reply_json(stream, 201, &result)
                    } else if result["error"] == "invalid_workspace" {
                        reply_error(stream, 400, Envelope::Plain, "invalid_workspace")
                    } else {
                        reply_error(stream, 500, Envelope::Plain, "creation_failed")
                    }
                }
                crate::Ipc::NoDaemon => reply_error(stream, 503, Envelope::Plain, "desktop_not_running"),
                crate::Ipc::Stalled => reply_error(stream, 504, Envelope::Plain, "creation_timeout_check_machine_before_retry"),
            };
        }
    }

    // Dynamic harness routes: /api/v1/harnesses/<id>/stream (WebSocket, live
    // pane output), /keys (phone → harness input), and DELETE (close harness).
    // Only our own `sd_term_*` sessions are addressable, so a paired phone
    // cannot type into unrelated tmux sessions.
    if let Some(rest) = path.strip_prefix("/api/v1/harnesses/") {
        if req.method == "DELETE" && !rest.contains('/') {
            return handle_close_harness(stream, req, rest);
        }
        if let Some((id, action)) = rest.rsplit_once('/') {
            if id.starts_with("sd_term_") {
                match (req.method.as_str(), action) {
                    ("GET", "input") => {
                        if !require_live_session(stream, req, id, Envelope::Plain) {
                            return;
                        }
                        if ws_upgrade(stream, req) { return stream_keys(stream, id); }
                        return reply_error(stream, 400, Envelope::Plain, "expected_websocket");
                    }
                    ("GET", "stream") => {
                        if !require_pairing(stream, &req.headers, Envelope::Status) {
                            return;
                        }
                        return match ws_upgrade(stream, req) {
                            true => terminal_stream::stream(stream, id, terminal_stream::ansi_only(&req.query), terminal_stream::viewport_requested(&req.query)),
                            false => reply_error(stream, 400, Envelope::Status, "expected_websocket"),
                        };
                    }
                    ("GET", "prompts") => return handle_prompts(stream, req, id),
                    ("POST", "keys") => return handle_keys(stream, req, id),
                    ("POST", "editor-action") => return handle_editor_action(stream, req, id),
                    ("POST", "relaunch") => return handle_relaunch_harness(stream, req, id),
                    _ => {}
                }
            }
        }
    }

    // Live terminal bytes for one owned card: host output and viewer keystrokes
    // are WSS binary frames, plus bounded text control frames.
    if let Some(rest) = path.strip_prefix("/api/v1/desktop/terminals/") {
        if let Some(card_id) = rest.strip_suffix("/attach") {
            if req.method != "GET" || card_id.contains('/') {
                return reply_error(stream, 404, Envelope::Plain, "not_found");
            }
            if !require_pairing(stream, &req.headers, Envelope::Plain) {
                return;
            }
            // Ownership, session liveness, the host-owned grid and this
            // device's attachment budget are all settled before the upgrade, so
            // every failure is a status a viewer can show.
            let device = stream.credential_id().map(str::to_string);
            let target = match desktop::resolve_attach(card_id, device.as_deref()) {
                Ok(target) => target,
                Err(error) => return reply_error(stream, error.code, Envelope::Plain, error.error),
            };
            if !ws_upgrade(stream, req) {
                return reply_error(stream, 400, Envelope::Plain, "expected_websocket");
            }
            return desktop::attach_terminal(stream, target);
        }
    }

    // One typed command route for every mutation. The old per-card
    // `cards/<id>/position` route is gone: a move that cannot name the revision
    // it was based on is exactly the edit this route exists to refuse.
    if path == "/api/v1/desktop/commands" {
        // Authorization comes before the method check, like every other desktop
        // route: an unpaired caller learns nothing about the route's shape.
        if !require_pairing(stream, &req.headers, Envelope::Plain) {
            return;
        }
        if req.method != "POST" {
            return reply_error(stream, 405, Envelope::Plain, "method_not_allowed");
        }
        // Deduplication is per credential, so a revoked device cannot replay
        // another device's request id.
        let device = stream.credential_id().map(str::to_string).unwrap_or_default();
        return desktop::command(stream, &device, &req.body);
    }

    match (req.method.as_str(), path) {
        ("GET", "/api/v1/desktop/capabilities") => {
            if !require_pairing(stream, &req.headers, Envelope::Plain) {
                return;
            }
            let machine_id = pair_state().lock().unwrap().cfg.bridge_id.clone();
            let capabilities = crate::desktop_protocol::Capabilities::current(machine_id);
            reply_json(stream, 200, &serde_json::to_value(capabilities).unwrap());
        }
        ("GET", "/api/v1/desktop/workspace" | "/api/v1/desktop/events") => {
            if !require_pairing(stream, &req.headers, Envelope::Plain) {
                return;
            }
            if path.ends_with("/events") {
                // The budget is settled before the upgrade, so a refusal is a
                // status a viewer can show, and it is released with the stream.
                let device = stream.credential_id().map(str::to_string).unwrap_or_default();
                let Some(subscription) = desktop_events::subscribe(&device) else {
                    return reply_error(stream, 429, Envelope::Plain, "subscription_limit");
                };
                if !ws_upgrade(stream, req) {
                    return reply_error(stream, 400, Envelope::Plain, "expected_websocket");
                }
                desktop_events::serve(stream, subscription);
            } else {
                desktop::get_workspace(stream);
            }
        }
        // The daemon's liveness probe (every 5 s, owner-only socket) needs no
        // address discovery: that ran `ip` whenever its cache had expired.
        // Older bridges ignore the query and answer the full ping.
        ("GET", "/api/v1/ping") if local && req.query.split('&').any(|pair| pair == HEALTH_QUERY) => {
            reply_json(stream, 200, &health_body())
        }
        ("GET", "/api/v1/ping") => {
            let bridge_id = pair_state().lock().unwrap().cfg.bridge_id.clone();
            let addresses = bridge_addresses();
            let tailscale_ip = addresses.iter().find(|v| v["kind"] == "tailscale" && v["connectable"] == true).map(|v| v["address"].clone());
            reply_json(
            stream,
            200,
            &serde_json::json!({
                "status": "ok",
                "service": SERVICE_NAME,
                "protocolVersion": PROTOCOL_VERSION,
                "hostname": hostname(),
                "lanIp": lan_ip(),
                "bridgeId": bridge_id,
                "addresses": addresses,
                "tailscaleIp": tailscale_ip,
                "port": BRIDGE_PORT,
                "time": utc_now_iso(),
            }),
        ) },
        ("GET", "/api/v1/harnesses") => {
            if !require_pairing(stream, &req.headers, Envelope::Status) {
                return;
            }
            reply_body(stream, 200, &harness_list::current_document());
        }
        ("POST", "/api/v1/completions") => {
            if !require_pairing(stream, &req.headers, Envelope::Plain) {
                return;
            }
            let body: serde_json::Value = serde_json::from_str(&req.body).unwrap_or_default();
            let ids: Option<Vec<String>> = body["sessions"].as_array().filter(|v| v.len() <= 32).and_then(|v| {
                v.iter().map(|id| id.as_str().filter(|s| !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')).map(str::to_string)).collect()
            });
            let Some(ids) = ids else {
                return reply_error(stream, 400, Envelope::Plain, "invalid_sessions");
            };
            completions::handle(stream, &body, &ids);
        }
        ("GET", "/api/v1/theme") => {
            if !require_pairing(stream, &req.headers, Envelope::Status) {
                return;
            }
            reply_json(stream, 200, &theme_document());
        }
        ("GET", "/api/v1/transcribe") => {
            if !require_pairing(stream, &req.headers, Envelope::Plain) {
                return;
            }
            reply_json(stream, 200, &crate::transcribe::status_json());
        }
        // Full document on connect, then on every change (1s poll).
        ("GET", "/api/v1/harnesses/stream") => {
            if !require_pairing(stream, &req.headers, Envelope::Status) {
                return;
            }
            if !ws_upgrade(stream, req) {
                return reply_error(stream, 400, Envelope::Status, "expected_websocket");
            }
            harness_list::stream(stream);
        }
        _ => reply_error(stream, 404, Envelope::Plain, "not found"),
    }
}

/// Blocking serve loop for `super-desktop harness-bridge`.
pub fn serve(port: u16) {
    let tls = security::tls_config().expect("Cannot load TLS identity; refusing insecure fallback");
    let listener = match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{SERVICE_NAME}: failed to bind 0.0.0.0:{port}: {e}");
            std::process::exit(1);
        }
    };
    security::serve_control().expect("Cannot start private desktop control socket");
    println!("{SERVICE_NAME} HTTPS on 0.0.0.0:{port} (lan {})", lan_ip());
    // Advertise for the launcher's NSD lookup.
    // On a worker thread: confirming the record with `avahi-browse` takes a
    // moment, and discovery must never delay serving requests.
    std::thread::spawn(move || {
        if !publish_mdns(port) {
            eprintln!(
                "{SERVICE_NAME}: mDNS {MDNS_SERVICE_TYPE} not advertised (avahi-utils missing?) — \
                 the launcher can still be pointed at {}:{port} manually",
                lan_ip()
            );
        }
    });
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                if let Some(admission) = security::Admission::acquire(&s) {
                    let tls = tls.clone();
                    std::thread::spawn(move || {
                        if let Ok(stream) = Connection::tls(s, tls) { serve_connection(stream, Some(admission)); }
                    });
                }
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
}

