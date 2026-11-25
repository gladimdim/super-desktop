use super::*;

pub(in crate::window) struct CardRefusals {
    pub(in crate::window) unavailable: &'static str,
    pub(in crate::window) not_found: &'static str,
    /// The request's expected epoch or revision is out of date.
    pub(in crate::window) stale: &'static str,
    pub(in crate::window) no_widget: &'static str,
    /// The card, its widget and its session do not match one to one.
    pub(in crate::window) ambiguous: &'static str,
    /// Refuse a card whose session is being closed, as ambiguous.
    pub(in crate::window) refuse_closing: bool,
    /// Refuse a card in the middle of a drag or resize, with this message.
    pub(in crate::window) busy: Option<&'static str>,
}

/// The card a guarded CLI request names: its snapshot entry, its widget and
/// that widget's data, all checked against one another.
pub(in crate::window) struct GuardedCard {
    pub(in crate::window) current: crate::desktop_protocol::DesktopCard,
    pub(in crate::window) card: Rc<MiniTerminalCard>,
    pub(in crate::window) data: TerminalData,
}


impl SuperDesktopWindow {
    /// Read current local layout without presenting the window or probing tmux.
    /// This remains the local workspace even when a remote view is added later.
    pub fn desktop_snapshot(&self, model: &crate::workspace_model::LocalWorkspace)
        -> Result<crate::desktop_protocol::LocalWorkspaceSnapshot, &'static str>
    {
        let presentation = self.terminal_cards.borrow().iter().map(|card| {
            (card.data.borrow().id.clone(), card.desktop_presentation())
        }).collect();
        // Export the same logical canvas used by local placement/animation.
        // Do not export animated widget coordinates during slide-in/out.
        let canvas = crate::desktop_protocol::Canvas {
            x: 0, y: 0, width: self.screen_width() as u32, height: self.screen_height() as u32,
            scale: self.window.scale_factor() as f64,
            top_inset: top_bar_height(self.state.borrow().top_bar_size) as u32,
        };
        model.snapshot(canvas, &presentation)
    }

    /// The snapshot entry of the card a CLI request names.
    pub(in crate::window) fn cli_card_entry(
        &self,
        model: &crate::workspace_model::LocalWorkspace,
        request: &crate::control::Request,
        card_id: &str,
        refusals: &CardRefusals,
    ) -> Result<(crate::desktop_protocol::LocalWorkspaceSnapshot, crate::desktop_protocol::DesktopCard), crate::control::Reply> {
        let fail = |code, message| crate::control::Reply::failure(&request.request_id, code, message);
        let snapshot = self.desktop_snapshot(model).map_err(|_| fail("unavailable", refusals.unavailable))?;
        let current = snapshot.cards.iter().find(|card| card.card_id == card_id).cloned()
            .ok_or_else(|| fail("not_found", refusals.not_found))?;
        Ok((snapshot, current))
    }

    /// The live card behind a snapshot entry: exactly one widget and one
    /// session must match it, and the handler decides whether a closing card
    /// or one in the middle of a drag or resize is refused.
    pub(in crate::window) fn cli_card_widget(
        &self,
        request: &crate::control::Request,
        snapshot: &crate::desktop_protocol::LocalWorkspaceSnapshot,
        current: &crate::desktop_protocol::DesktopCard,
        refusals: &CardRefusals,
    ) -> Result<(Rc<MiniTerminalCard>, TerminalData), crate::control::Reply> {
        let fail = |code, message| crate::control::Reply::failure(&request.request_id, code, message);
        let card = self.any_terminal_card(&current.card_id).map_err(|_| fail("not_found", refusals.no_widget))?;
        let data = card.data.borrow().clone();
        let entries = |same: &dyn Fn(&crate::desktop_protocol::DesktopCard) -> bool| {
            snapshot.cards.iter().filter(|card| same(card)).count()
        };
        let widgets = self.terminal_cards.borrow().iter()
            .filter(|other| other.data.borrow().id == current.card_id).count();
        if entries(&|card| card.card_id == current.card_id) != 1
            || entries(&|card| card.session_name == current.session_name) != 1
            || data.session_name != current.session_name
            || widgets != 1
            || (refusals.refuse_closing && card.cli_session_task().is_closed())
        {
            return Err(fail("conflict", refusals.ambiguous));
        }
        if let Some(message) = refusals.busy {
            if card.is_being_dragged() || card.container.has_css_class("term-resizing") {
                return Err(fail("conflict", message));
            }
        }
        Ok((card, data))
    }

    /// The card a guarded CLI request names, checked against the epoch and
    /// revision the request expects, when it carries them.
    pub(in crate::window) fn guarded_card(
        &self,
        model: &crate::workspace_model::LocalWorkspace,
        request: &crate::control::Request,
        card_id: &str,
        expect: Option<(&String, &String)>,
        refusals: &CardRefusals,
    ) -> Result<GuardedCard, crate::control::Reply> {
        let (snapshot, current) = self.cli_card_entry(model, request, card_id, refusals)?;
        if let Some((epoch, revision)) = expect {
            if epoch != &snapshot.epoch || revision != &crate::control_geometry::revision(&snapshot, &current) {
                return Err(crate::control::Reply::failure(&request.request_id, "conflict", refusals.stale));
            }
        }
        let (card, data) = self.cli_card_widget(request, &snapshot, &current, refusals)?;
        Ok(GuardedCard { current, card, data })
    }

    pub(crate) fn cli_file_target(
        &self,
        model: &crate::workspace_model::LocalWorkspace,
        request: &crate::control::Request,
    ) -> Result<crate::state::TerminalData, crate::control::Reply> {
        use crate::control::{Command, Reply};
        let (id, expect) = match &request.command {
            Command::Files { id, .. } => (id, None),
            Command::FilesEdit { id, expect_epoch, expect_revision, .. } => (id, Some((expect_epoch, expect_revision))),
            _ => return Err(Reply::failure(&request.request_id, "invalid_request", "Expected terminal files request.")),
        };
        let refusals = CardRefusals {
            unavailable: "Workspace unavailable.",
            not_found: "No unique local terminal card has that ID.",
            stale: "Card or display changed; read terminal geometry again.",
            no_widget: "Local terminal widget missing.",
            ambiguous: "Terminal identity is ambiguous or closing.",
            refuse_closing: true,
            busy: None,
        };
        Ok(self.guarded_card(model, request, id, expect, &refusals)?.data)
    }

    /// Read actual note buffers, including edits waiting for autosave.
    pub(in crate::window) fn cli_live_state(&self) -> AppState {
        let mut state = self.state.borrow().clone();
        state.notes = self.note_cards.borrow().iter().map(|note| note.live_data()).collect();
        state
    }

    pub(in crate::window) fn cli_layout_ready(&self, layout: &crate::control::Layout) -> Result<(), &'static str> {
        if self.slide.running.get() {return Err("Workspace animation is in progress.");}
        for item in &layout.items {
            if item.kind=="note" {
                let cards:Vec<_>=self.note_cards.borrow().iter().filter(|n|n.data.borrow().id==item.id).cloned().collect();
                if cards.len()!=1 || cards[0].text_view.has_focus() || cards[0].container.has_css_class("dragging") || cards[0].container.has_css_class("resizing") {return Err("A note is missing, ambiguous or being edited.");}
            } else if matches!(item.kind.as_str(),"editor"|"plugin") {
                let extra=crate::control_workspace::extra_cards();
                let card=extra.get(&(item.kind.clone(),item.id.clone())).ok_or("Editor or plugin card unavailable.")?;
                if card.container.has_css_class("resizing") || card.container.has_css_class("dragging") {
                    return Err("An editor or plugin card is being moved.");
                }
            } else {
                let cards:Vec<_>=self.terminal_cards.borrow().iter().filter(|n|n.data.borrow().id==item.id).cloned().collect();
                if cards.len()!=1 || cards[0].is_being_dragged() || cards[0].container.has_css_class("term-resizing") {return Err("A terminal is missing, ambiguous or being moved.");}
            }
        }
        Ok(())
    }

    pub(crate) fn cli_workspace(&self, model: &crate::workspace_model::LocalWorkspace,
        request: &crate::control::Request) -> crate::control::Reply
    {
        if let Some(reply)=self.cli_preferences(model,request) { return reply; }
        use crate::control::{Command, Reply, WorkspaceEdit as Edit, WorkspaceQuery as Query};
        use crate::control_workspace as workspace;
        use serde_json::json;
        let fail = |code, message| Reply::failure(&request.request_id, code, message);
        let snapshot = match self.desktop_snapshot(model) {
            Ok(s) => s, Err(_) => return fail("unavailable", "Local workspace is unavailable."),
        };
        let state = self.cli_live_state();
        let envelope = |state: &AppState| json!({"epoch":snapshot.epoch,"revision":workspace::revision(state,&snapshot),
            "revisionScope":"workspace","units":"logical-pixels","canvas":snapshot.canvas});
        if let Command::Workspace { query } = &request.command {
            let mut data = envelope(&state);
            match query {
                Query::Inspect => { data["workspace"] = json!(crate::state::effective_workspace_dir(&state));
                    data["notesCount"] = json!(state.notes.len()); data["terminalsCount"] = json!(state.terminals.len()); },
                Query::Buffer {editor,document,offset} => match crate::editor::control::read(editor,document,*offset) {
                    Ok(buffer) => data["buffer"]=buffer, Err(e)=>return fail("invalid_arguments",e),
                },
                Query::DesktopLayout => data["layout"] = json!(workspace::desktop_layout(&state,&snapshot)),
                Query::Editors => data["editors"] = crate::editor::inventory(),
                Query::Folders => data["folders"] = self.workspace_choices(),
                Query::Layout => data["layout"] = json!(crate::control_workspace::export(&state,&snapshot)),
                Query::ValidateLayout {layout} => {
                    if let Err(message)=crate::control_workspace::validate_layout(layout,&state,&snapshot) { return fail("invalid_arguments",message); }
                    if let Err(message)=self.cli_layout_ready(layout) { return fail("conflict",message); }
                    data["valid"]=json!(true); data["layout"]=json!(layout);
                },
                Query::Notes => data["notes"] = json!(state.notes.iter().map(|n| workspace::note(n,false)).collect::<Vec<_>>()),
                Query::Note { id } => {
                    let matches:Vec<_> = state.notes.iter().filter(|n| n.id == *id).collect();
                    if matches.len()!=1 { return fail("not_found","No unique local note has that ID."); }
                    data["note"] = workspace::note(matches[0],true);
                },
            }
            return Reply::success(&request.request_id,data);
        }
        if let Err(reply) = workspace::check(request,&state,&snapshot,&snapshot.epoch) { return reply; }
        if self.slide.running.get() { return fail("conflict","A workspace animation is in progress."); }
        let Command::WorkspaceEdit {edit,..} = &request.command else { unreachable!() };
        let mut result = json!({});
        if let Edit::Editor {editor,action} = edit {
            match crate::editor::control::edit(editor,action) {
                Ok(info)=>result=info,Err(e)=>return fail("conflict",&e),
            }
        } else if let Edit::OpenEditor {folder,file} = edit {
            let folder = std::path::Path::new(folder);
            if !folder.is_absolute() || file.as_ref().is_some_and(|f| !folder.join(f).is_file()) {
                return fail("invalid_arguments","Use an absolute folder and an existing file.");
            }
            if let Err(message) = crate::editor::open(folder,file.as_deref().map(std::path::Path::new)) {
                return fail("unavailable",&message);
            }
        } else if let Edit::PluginSurface {plugin,surface,action} = edit {
            if !matches!(action.as_str(), "panel.open" | "card.open" | "card.new" | "fullscreen.open") {
                return fail("invalid_arguments","Use panel.open, card.open, card.new or fullscreen.open.");
            }
            let Some(slots) = crate::plugin_ui::slots::get() else { return fail("unavailable","Plugin host unavailable."); };
            if let Err(message) = slots.act(plugin,action,surface,"") {return fail("unavailable",&message);}
        } else if matches!(edit, Edit::ArrangeDesktop) {
            if let Err(message) = self.cli_layout_ready(&workspace::desktop_layout(&state,&snapshot)) {return fail("conflict",message);}
            self.auto_arrange();
        } else if let Edit::Navigate {direction} = edit {
            match direction.as_str() {
                "next" => self.cycle_local_cards(true),
                "previous" => self.cycle_local_cards(false),
                "attention" => self.jump_to_attention_card(),
                _ => return fail("invalid_arguments","Use next, previous or attention."),
            }
        } else if let Edit::ForgetFolder {path} = edit {
            self.state.borrow_mut().recent_dirs.retain(|dir| dir != path);
        } else if matches!(edit,Edit::Layout {..}|Edit::Arrange) {
            let layout=match edit {
                Edit::Layout {layout} => layout.clone(),
                _ => match workspace::arrange(&state,&snapshot) {Ok(v)=>v,Err(message)=>return fail("out_of_bounds",message)},
            };
            if let Err(message)=workspace::validate_layout(&layout,&state,&snapshot) { return fail("invalid_arguments",message); }
            if let Err(message)=self.cli_layout_ready(&layout) { return fail("conflict",message); }
            let extra=workspace::extra_cards();
            let host=crate::plugin_ui::slots::get().and_then(|s|s.canvas_host());
            for item in &layout.items {
                if item.kind=="note" {
                    let note=self.note_cards.borrow().iter().find(|n| n.data.borrow().id==item.id).cloned().unwrap();
                    {let mut data=note.data.borrow_mut();data.x=item.x;data.y=item.y;data.width=item.width;data.height=item.height;}
                    note.container.set_size_request(item.width,item.height);
                    self.canvas.move_(&note.container,item.x as f64,item.y as f64);
                    if let Some(data)=self.state.borrow_mut().notes.iter_mut().find(|n|n.id==item.id) {*data=note.live_data();}
                } else if let Some(card)=extra.get(&(item.kind.clone(),item.id.clone())) {
                    let Some(host)=&host else { return Reply::unknown(&request.request_id); };
                    card.set_geometry(host,crate::plugin_ui::cards::Geometry {x:item.x,y:item.y,width:item.width,height:item.height});
                } else {
                    let card=self.any_terminal_card(&item.id).unwrap();
                    let applied=if item.mode=="minimized" {self.move_terminal_card(&item.id,item.x,item.y).is_ok()} else {
                        card.apply_geometry(crate::card_resize::Rect {x:item.x as f64,y:item.y as f64,width:item.width,height:item.height})
                    };
                    if !applied {return Reply::unknown(&request.request_id);}
                }
            }
            self.ghosts.refresh();
            result["layout"]=json!(layout);
        } else if let Edit::Folder { path } = edit {
            if !std::path::Path::new(path).is_absolute() || path.len()>4096 || path.chars().any(char::is_control) {
                return fail("invalid_arguments","Use an absolute existing folder path without control characters.");
            }
            let Some(directory) = crate::state::clean_dir(path) else { return fail("invalid_arguments","Folder does not exist."); };
            { let mut state = self.state.borrow_mut(); state.workspace_dir=Some(directory.clone()); crate::state::remember_workspace_dir(&mut state,&directory); }
            self.ws_bar.show_folder(&directory);
            result["workspace"] = json!(directory);
        } else {
            let (id, mut updated, old) = match edit {
                Edit::NoteCreate { text,x,y,width,height,tag } => {
                    if state.notes.len()>=256 { return fail("limit_exceeded","At most 256 notes can be created through the CLI."); }
                    let id=format!("note_cli_{}",request.request_id);
                    if state.notes.iter().any(|n| n.id==id) { return fail("conflict","Reserved note ID already exists."); }
                    (id.clone(),Some(NoteData {id,text:text.clone(),x:*x,y:*y,width:*width,height:*height,tag:*tag,
                        color:"omarchy".into(),updated_at:0.0}),None)
                },
                Edit::NoteUpdate {id,..} | Edit::NoteDelete {id} | Edit::NoteMove {id,..} | Edit::NoteResize {id,..} | Edit::NoteTag {id,..} => {
                    let cards:Vec<_>=self.note_cards.borrow().iter().filter(|n| n.data.borrow().id==*id).cloned().collect();
                    if cards.len()!=1 { return fail("not_found","No unique local note has that ID."); }
                    let card=Rc::clone(&cards[0]);
                    if card.text_view.has_focus() || card.container.has_css_class("dragging") || card.container.has_css_class("resizing") {
                        return fail("conflict","The note is being edited or moved on the desktop.");
                    }
                    let mut note=card.live_data();
                    match edit {
                        Edit::NoteUpdate {text,..} => note.text=text.clone(),
                        Edit::NoteMove {x,y,..} => {note.x=*x; note.y=*y;},
                        Edit::NoteResize {width,height,..} => {note.width=*width;note.height=*height;},
                        Edit::NoteTag {tag,..} => note.tag=*tag,
                        _ => {}
                    }
                    (id.clone(), if matches!(edit,Edit::NoteDelete {..}) {None} else {Some(note)},Some(card))
                },
                _ => unreachable!()
            };
            if let Some(note) = &mut updated {
                if let Err(message)=workspace::validate_note(note,&snapshot.canvas,true) {
                    return fail("invalid_arguments",message);
                }
                note.updated_at=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64();
            }
            if let Some(old)=old {
                old.cancel_pending_save();
                self.note_cards.borrow_mut().retain(|n| n.data.borrow().id!=id);
                self.drag_pending.borrow_mut().remove(old.container.upcast_ref::<gtk4::Widget>());
                self.canvas.remove(&old.container);
            }
            self.state.borrow_mut().notes.retain(|n| n.id!=id);
            if let Some(note)=updated {
                result["note"] = workspace::note(&note,false);
                self.state.borrow_mut().notes.push(note.clone());
                self.spawn_note_widget(note,false);
            }
            result["id"] = json!(id);
            result["deleted"] = json!(matches!(edit,Edit::NoteDelete {..}));
        }
        crate::state::save_state_async(self.state.borrow().clone());
        let mut data=envelope(&self.cli_live_state());
        data["result"]=result; data["outcome"]=json!("applied");
        Reply::success(&request.request_id,data)
    }

    pub(in crate::window) fn cli_card_action(&self, model: &crate::workspace_model::LocalWorkspace, request: &crate::control::Request) -> crate::control::Reply {
        use crate::control::{CardAction, Command, Reply};
        let fail = |code, message| Reply::failure(&request.request_id, code, message);
        let Command::CardAction { id, action, expect_epoch, expect_revision } = &request.command else {
            return fail("invalid_request", "Expected a card action.");
        };
        const BUSY: &str = "A local gesture or animation is in progress.";
        let refusals = CardRefusals {
            unavailable: "Workspace unavailable.",
            not_found: "No local card has that ID.",
            stale: "Card or display changed; read terminal geometry again.",
            no_widget: "Card widget is unavailable.",
            ambiguous: "Card identity is ambiguous.",
            refuse_closing: false,
            busy: Some(BUSY),
        };
        let GuardedCard { current, card, .. } =
            match self.guarded_card(model, request, id, Some((expect_epoch, expect_revision)), &refusals) {
                Ok(guarded) => guarded,
                Err(reply) => return reply,
            };
        if self.slide.running.get() {
            return fail("conflict", BUSY);
        }
        match action {
            CardAction::Raise => card.cli_raise(),
            CardAction::Focus => {
                if !self.window.is_visible()
                    || self.machine_view.is_remote()
                    || current.layout.iconified && !current.expanded
                    || self.overlay_panels.iter().any(|p| p.is_visible())
                    || self.pairing_requests.is_open()
                    || self.pairing_wizard.is_open()
                {
                    return fail("invalid_state", "Focus requires a visible local workspace, restored card and closed dialogs. Use show and terminal restore first.");
                }
                if !card.cli_focus() {
                    return Reply::unknown(&request.request_id);
                }
            }
            CardAction::Tag { value } => {
                if *value > 8 {
                    return fail("invalid_arguments", "Tag must be 0..8.");
                }
                let before = card.data.borrow().clone();
                card.cli_set_tag(*value);
                let after = card.data.borrow().clone();
                persist(&self.state, |state| {
                    crate::folder_colors::note_card_saved(state, Some(&before), &after);
                    if let Some(saved) = state.terminals.iter_mut().find(|c| c.id == *id) {
                        saved.tag = *value;
                    }
                });
            }
        }
        let after = match self.desktop_snapshot(model) {
            Ok(v) => v,
            Err(_) => return Reply::unknown(&request.request_id),
        };
        let Some(current) = after.cards.iter().find(|c| c.card_id == *id) else {
            return Reply::unknown(&request.request_id);
        };
        let mut data = crate::control_geometry::describe(&after, current);
        data["tag"] = serde_json::json!(card.data.borrow().tag);
        data["action"] = serde_json::json!(action);
        data["outcome"] = serde_json::json!("applied");
        data["compositorFocusObserved"] = serde_json::json!(false);
        Reply::success(&request.request_id, data)
    }

    /// Local CLI geometry uses the live card and output on this GTK turn.
    /// It never sends a bridge command, starts a session, or presents the overlay.
    pub fn cli_geometry(&self, model: &crate::workspace_model::LocalWorkspace, request: &crate::control::Request) -> crate::control::Reply {
        if matches!(request.command,crate::control::Command::CardAction {..}) {return self.cli_card_action(model,request);}
        use crate::control::{Command, Reply};
        use crate::control_geometry as geometry;
        let id = &request.request_id;
        let card_id = geometry::card_id(&request.command);
        let refusals = CardRefusals {
            unavailable: "Local geometry is unavailable.",
            not_found: "No local terminal card has that ID.",
            // `geometry::prepare` checks the expected epoch and revision.
            stale: "",
            no_widget: "No local terminal widget has that ID.",
            ambiguous: "The card-to-session mapping is ambiguous or changed.",
            refuse_closing: false,
            busy: Some("A local geometry gesture is in progress."),
        };
        let (snapshot, current) = match self.cli_card_entry(model, request, card_id, &refusals) {
            Ok(entry) => entry,
            Err(reply) => return reply,
        };
        if matches!(request.command, Command::Geometry { .. }) {
            return Reply::success(id, geometry::describe(&snapshot, &current));
        }
        let mode_change = if matches!(request.command, Command::Mode { .. }) {
            match geometry::prepare_mode(request, &snapshot, &current) {
                Ok(changed) => Some(changed),
                Err(reply) => return reply,
            }
        } else { None };
        let prepared = if mode_change.is_none() {
            match geometry::prepare(request, &snapshot, &current) {
                Ok(prepared) => Some(prepared),
                Err(reply) => return reply,
            }
        } else { None };
        let card = match self.cli_card_widget(request, &snapshot, &current, &refusals) {
            Ok((card, _)) => card,
            Err(reply) => return reply,
        };
        let applied = match request.command {
            Command::Mode { action, .. } => {
                if mode_change == Some(false) { true } else {
                    let changed = card.cli_set_mode(action, self.screen_width(), self.screen_height());
                    if changed {
                        // Minimize/restore already position and persist through
                        // the card's save callback, including display clamping.
                        match action {
                            crate::control::ModeAction::Expand => {
                                place_expanded(&self.canvas, &card, (self.screen_width(), self.screen_height()));
                            }
                            crate::control::ModeAction::Collapse => place_at_rest(&self.canvas, &card),
                            _ => {}
                        }
                        self.ghosts.refresh();
                    }
                    changed
                }
            }
            Command::Move { .. } => {
                let rect = prepared.as_ref().unwrap().rect;
                self.move_terminal_card(card_id, rect.x as i32, rect.y as i32).is_ok()
            }
            Command::Resize { .. } => card.apply_geometry(prepared.as_ref().unwrap().rect),
            _ => false,
        };
        if !applied {
            return Reply::unknown(id);
        }
        let after = match self.desktop_snapshot(model) {
            Ok(snapshot) => snapshot,
            Err(_) => return Reply::unknown(id),
        };
        let Some(current) = after.cards.iter().find(|card| card.card_id == card_id) else {
            return Reply::unknown(id);
        };
        let mut data = geometry::describe(&after, current);
        if let Some(prepared) = prepared {
            data["requested"] = prepared.requested;
            data["clamped"] = serde_json::json!(prepared.clamped);
        } else if let Command::Mode { action, .. } = &request.command {
            data["requested"] = serde_json::json!({"action":action});
            data["changed"] = serde_json::json!(mode_change.unwrap());
            data["attachmentObserved"] = serde_json::json!(false);
        }
        data["outcome"] = serde_json::json!("applied");
        Reply::success(id, data)
    }

    /// The close worker owns destruction; GTK only validates and removes the
    /// exact card. This path deliberately does not call close_session().
    pub(crate) fn cli_close(&self, model: &crate::workspace_model::LocalWorkspace,
        request: &crate::control::Request, action: crate::control_close::Action) -> crate::control_close::UiResult
    {
        use crate::control::{Command, Reply};
        use crate::control_close::{Action, Target};
        let fail = |code, message| Reply::failure(&request.request_id, code, message);
        let (id, expect_epoch, expect_revision) = match &request.command {
            Command::Forget { id, expect_epoch, expect_revision }
            | Command::Relaunch { id, expect_epoch, expect_revision, .. }
            | Command::Viewport { id, expect_epoch: Some(expect_epoch), expect_revision: Some(expect_revision), .. }
            | Command::Attach { id, expect_epoch, expect_revision, .. }
            | Command::Close { id, expect_epoch, expect_revision, .. }
            | Command::Input { id, expect_epoch, expect_revision, .. } => (id, expect_epoch, expect_revision),
            _ => return Err(fail("invalid_request", "Expected a guarded terminal operation.")),
        };
        let refusals = CardRefusals {
            unavailable: "Local workspace is unavailable.",
            not_found: "No local terminal card has that ID.",
            stale: "The card changed or the daemon restarted. Read terminal geometry again.",
            no_widget: "No local terminal widget has that ID.",
            ambiguous: "The card-to-session mapping changed, is ambiguous, or is closing.",
            refuse_closing: true,
            busy: Some("A local geometry gesture is in progress; the card was not closed."),
        };
        let GuardedCard { card, data, .. } =
            self.guarded_card(model, request, id, Some((expect_epoch, expect_revision)), &refusals)?;
        match action {
            Action::Inspect => Ok(Some(Target { data, task: card.cli_session_task() })),
            Action::Remove(expected) => {
                if !matches!(request.command, Command::Close { .. } | Command::Forget {..} | Command::Relaunch {..}) {
                    return Err(fail("invalid_request", "Only close may remove a card."));
                }
                if data.id != expected.data.id || data.session_name != expected.data.session_name
                    || data.created_at != expected.data.created_at
                    || data.command != expected.data.command || data.workspace_dir != expected.data.workspace_dir
                    || !std::sync::Arc::ptr_eq(&card.cli_session_task(), &expected.task) {
                    return Err(fail("conflict", "The terminal card was replaced before close."));
                }
                card.detach_for_cli_close();
                self.terminal_cards.borrow_mut().retain(|other| other.data.borrow().id != *id);
                self.drag_pending.borrow_mut().remove(card.container.upcast_ref::<gtk4::Widget>());
                self.canvas.remove(&card.container);
                persist(&self.state, |state| {
                    state.terminals.retain(|other| other.id != *id);
                    crate::state::normalize_terminal_order(state);
                });
                self.ghosts.refresh();
                Ok(None)
            }
        }
    }

}
