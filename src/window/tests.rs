use super::*;

// A 1000×700 workspace below a 58 px top bar, 128 px icons, 12 px gaps:
// seven icons fit in a row (12 + 7×140 = 992), rows start at y=560.
const AREA: IconArea = IconArea { left: 12, top: 70, right: 988, bottom: 688 };

#[test]
fn icon_slots_start_bottom_left() {
    assert_eq!(first_free_icon_slot(&[], AREA, 128, 12), (12, 560));
}

#[test]
fn icon_slots_fill_the_row_left_to_right_then_the_row_above() {
    let mut icons = Vec::new();
    let mut placed = Vec::new();
    for _ in 0..9 {
        let (x, y) = first_free_icon_slot(&icons, AREA, 128, 12);
        placed.push((x, y));
        icons.push((x, y, 128, 128));
    }
    assert_eq!(
        placed,
        vec![
            (12, 560), (152, 560), (292, 560), (432, 560), (572, 560), (712, 560), (852, 560),
            (12, 420), (152, 420),
        ]
    );
}

#[test]
fn icon_slots_reuse_a_gap_left_by_a_moved_icon() {
    // The second icon of the bottom row was dragged away.
    let icons = [(12, 560, 128, 128), (292, 560, 128, 128), (500, 200, 128, 128)];
    assert_eq!(first_free_icon_slot(&icons, AREA, 128, 12), (152, 560));
}

#[test]
fn icon_slots_skip_a_spot_an_icon_dropped_across() {
    // Dropped between two spots, it takes both.
    let icons = [(80, 600, 128, 128)];
    assert_eq!(first_free_icon_slot(&icons, AREA, 128, 12), (292, 560));
}

#[test]
fn icon_slots_share_bottom_left_when_full_or_tiny() {
    let mut icons = Vec::new();
    for row in 0..5 {
        for col in 0..7 {
            icons.push((12 + col * 140, 560 - row * 140, 128, 128));
        }
    }
    assert_eq!(first_free_icon_slot(&icons, AREA, 128, 12), (12, 560));
    let tiny = IconArea { left: 12, top: 70, right: 100, bottom: 150 };
    assert_eq!(first_free_icon_slot(&[], tiny, 128, 12), (12, 70));
}

#[test]
fn icon_slots_follow_the_icon_side_of_a_scaled_view() {
    assert_eq!(first_free_icon_slot(&[(12, 624, 64, 64)], AREA, 64, 12), (88, 624));
}

#[test]
fn antigravity_prompt_history_shortcut_opens_the_focused_cards_panel() {
    if !crate::gtk_test::is_child() {
        crate::gtk_test::run_in_child_process("window::tests::antigravity_prompt_history_shortcut_opens_the_focused_cards_panel");
        return;
    }
    let root = std::path::PathBuf::from(std::env::var_os("SD_TEST_ISOLATION_ROOT").unwrap());
    std::env::set_var("HOME", &root);
    std::env::set_var("XDG_CONFIG_HOME", &root);
    std::env::set_var("XDG_STATE_HOME", &root);
    gtk4::init().unwrap();
    let app = gtk4::Application::new(Some("com.superdesktop.HistoryTest"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gtk4::gio::Cancellable>).unwrap();
    let mut state = AppState { welcome_tour_seen: true, ..AppState::default() };
    let session = "sd_term_agy_history";
    state.terminals.push(serde_json::from_value(serde_json::json!({
        "id":session, "session_name":session, "agent_type":"antigravity", "command":"/bin/false",
        "x":20, "y":120, "width":400, "height":300, "created_at":0.0
    })).unwrap());
    let model = crate::workspace_model::LocalWorkspace::new(state);
    let window = SuperDesktopWindow::new(&app, || {}, Rc::new(crate::hotcorner::Zone::default()), model.state());
    window.show_again();
    let card = window.any_terminal_card(session).unwrap();
    card.focus_terminal();
    assert!(window.focused_local_card().is_some_and(|c| Rc::ptr_eq(&c, &card)));
    let controllers = window.window.observe_controllers();
    let modifiers = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK;
    let send = |key: gdk::Key, keycode: u32| {
        (0..controllers.n_items())
            .filter_map(|i| controllers.item(i)?.downcast::<EventControllerKey>().ok())
            .filter(|keys| keys.propagation_phase() == gtk4::PropagationPhase::Capture)
            .any(|keys| keys.emit_by_name::<bool>("key-pressed", &[&key, &keycode, &modifiers]))
    };
    assert!(send(gdk::Key::P, 0));
    // Where the test display supplies a keymap, exercise a non-Latin keysym
    // through the same window controller with the physical P keycode.
    let display = gtk4::prelude::WidgetExt::display(&window.window);
    if let Some(position) = display.map_keyval(gdk::Key::p).and_then(|keys| keys.into_iter().next()) {
        assert!(send(gdk::Key::Cyrillic_ZE, position.keycode()));
    }
    let tree = window.window.upcast_ref::<gtk4::Widget>();
    let drawer = crate::gtk_test::find_first::<gtk4::Box>(tree, "asset-drawer").unwrap();
    assert!(drawer.is_mapped());
    assert!(crate::gtk_test::find_where(drawer.upcast_ref(), |w|
        w.downcast_ref::<Label>().is_some_and(|label| label.text() == "Prompt history")).is_some());
    window.window.close();
}

#[test]
fn cli_notes_preserve_live_text_and_guard_edits() {
    crate::gtk_test::run_in_child_process("window::tests::cli_notes_inner");
}

#[test]
fn cli_notes_inner() {
    if !crate::gtk_test::is_child() { return; }
    use crate::control::{Command, Request, WorkspaceQuery as Query, WorkspaceEdit as Edit};
    let root=std::env::temp_dir().join(format!("sd-notes-window-{}",std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for name in ["HOME","XDG_CONFIG_HOME","XDG_STATE_HOME","XDG_CACHE_HOME","TMUX_TMPDIR"] { std::env::set_var(name,&root); }
    std::env::remove_var("TMUX"); std::env::remove_var("TMUX_PANE");
    gtk4::init().unwrap();
    let app=gtk4::Application::new(Some("com.superdesktop.CliNotesTest"),gtk4::gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gtk4::gio::Cancellable>).unwrap();
    let mut state=AppState::default(); state.notes.clear();
    let model=crate::workspace_model::LocalWorkspace::new(state);
    let window=SuperDesktopWindow::new(&app,||{},Rc::new(crate::hotcorner::Zone::default()),model.state());
    let read=|query| window.cli_workspace(&model,&Request {control_version:1,request_id:"read".into(),command:Command::Workspace {query}});
    let prepare=|edit,id:&str| {
        let data=read(Query::Inspect).data.unwrap();
        Request {control_version:1,request_id:id.into(),command:Command::WorkspaceEdit {edit,
            expect_epoch:data["epoch"].as_str().unwrap().into(),expect_revision:data["revision"].as_str().unwrap().into()}}
    };
    let create=prepare(Edit::NoteCreate {text:"one\nПривіт\t$(literal)".into(),x:80,y:140,width:260,height:200,tag:3},"create");
    let journal=root.join("receipts");
    let apply=|request:&Request| crate::control_geometry::dispatch(&journal,request,std::time::Instant::now()+Duration::from_secs(3),|r|Ok(window.cli_workspace(&model,r)));
    assert!(apply(&create).ok);
    assert!(apply(&create).ok,"duplicate returns receipt");
    assert_eq!(window.note_cards.borrow().len(),1);
    assert!(!read(Query::Notes).data.unwrap()["notes"][0].as_object().unwrap().contains_key("text"));
    let note_id="note_cli_create".to_string();
    assert_eq!(read(Query::Note {id:note_id.clone()}).data.unwrap()["note"]["text"],"one\nПривіт\t$(literal)");
    let stale=prepare(Edit::NoteUpdate {id:note_id.clone(),text:"overwrite".into()},"stale");
    let old=Rc::clone(&window.note_cards.borrow()[0]);
    old.text_view.buffer().set_text("unsaved desktop edit");
    assert_eq!(window.cli_workspace(&model,&stale).exit_code(),5,"pending autosave must invalidate revision");
    assert_eq!(read(Query::Note {id:note_id.clone()}).data.unwrap()["note"]["text"],"unsaved desktop edit");
    let update=prepare(Edit::NoteUpdate {id:note_id.clone(),text:"replacement\n".into()},"update");
    old.container.add_css_class("resizing");
    assert_eq!(window.cli_workspace(&model,&update).exit_code(),5);
    old.container.remove_css_class("resizing");
    assert!(apply(&update).ok);
    // Keep the retired widget alive past its former debounce deadline.
    let deadline=std::time::Instant::now()+Duration::from_millis(400);
    while std::time::Instant::now()<deadline { while glib::MainContext::default().iteration(false) {} std::thread::sleep(Duration::from_millis(10)); }
    assert_eq!(model.state().borrow().notes[0].text,"replacement\n");
    for (edit,id) in [(Edit::NoteMove {id:note_id.clone(),x:100,y:160},"move"),
        (Edit::NoteResize {id:note_id.clone(),width:300,height:220},"resize"),
        (Edit::NoteTag {id:note_id.clone(),tag:8},"tag")] {
        let reply=apply(&prepare(edit,id)); assert!(reply.ok,"{reply:?}");
    }
    let bad=prepare(Edit::NoteMove {id:note_id.clone(),x:i32::MAX,y:160},"bad");
    assert_eq!(apply(&bad).exit_code(),2);
    assert_eq!(model.state().borrow().notes[0].x,100);
    assert_eq!(model.state().borrow().notes[0].tag,8);
    let exported=read(Query::Layout).data.unwrap()["layout"].clone();
    let mut layout:crate::control::Layout=serde_json::from_value(exported).unwrap();
    layout.items[0].x=120;
    assert!(read(Query::ValidateLayout {layout:layout.clone()}).ok);
    layout.items.push(crate::control::LayoutItem {kind:"note".into(),id:"missing".into(),mode:"normal".into(),x:20,y:140,width:260,height:200});
    assert!(!apply(&prepare(Edit::Layout {layout:layout.clone()},"invalid-layout")).ok);
    assert_eq!(model.state().borrow().notes[0].x,100,"invalid later item must not move earlier item");
    layout.items.pop();
    assert!(apply(&prepare(Edit::Layout {layout},"layout")).ok);
    assert_eq!(model.state().borrow().notes[0].x,120);
    assert!(apply(&prepare(Edit::Arrange,"arrange")).ok);
    assert_eq!(model.state().borrow().notes[0].x,10);
    assert!(!window.window.is_visible());
    crate::state::flush_state_saves_checked().unwrap();
    assert_eq!(crate::state::load_state().notes[0].text,"replacement\n");
    assert!(apply(&prepare(Edit::NoteDelete {id:note_id.clone()},"delete")).ok);
    assert_eq!(read(Query::Note {id:note_id}).exit_code(),3);
    assert_eq!(window.note_cards.borrow().len(),0);
    assert!(crate::state::load_state().notes.is_empty());
    use crate::control::{PreferencesQuery as PQ,PreferencesEdit as PE,LauncherSpec};
    let pref_read=|query| window.cli_workspace(&model,&Request {control_version:1,request_id:"prefs-read".into(),command:Command::Preferences {query}});
    let pref_request=|edit,id:&str| {let data=read(Query::Inspect).data.unwrap();Request {control_version:1,request_id:id.into(),command:Command::PreferencesEdit {edit,expect_epoch:data["epoch"].as_str().unwrap().into(),expect_revision:data["revision"].as_str().unwrap().into()}}};
    assert!(pref_read(PQ::Settings {key:None}).ok);
    assert_eq!(pref_read(PQ::Settings {key:Some("unlisted".into())}).exit_code(),3);
    let secret="PRIVATE_ARGUMENT_LITERAL";
    let args=pref_request(PE::HarnessArgs {id:"claude".into(),arguments:Some(vec![secret.into()])},"args-set");
    assert!(apply(&args).ok);
    assert_eq!(crate::launch_args::effective("claude"),[secret]);
    assert_eq!(pref_read(PQ::HarnessArgs {id:"claude".into()}).data.unwrap()["arguments"][0],secret);
    assert!(!std::fs::read_to_string(journal.join("args-set.json")).unwrap().contains(secret));
    assert!(apply(&pref_request(PE::HarnessArgs {id:"claude".into(),arguments:None},"args-reset")).ok);
    assert_eq!(crate::launch_args::effective("claude"),crate::launch_args::builtin("claude"));
    assert_eq!(apply(&pref_request(PE::Setting {key:"toolbarSize".into(),value:Some(serde_json::json!("giant"))},"invalid-size")).exit_code(),2);
    for size in ["small","medium","large"] {
        assert!(apply(&pref_request(PE::Setting {key:"toolbarSize".into(),value:Some(serde_json::json!(size))},size)).ok);
        assert_eq!(pref_read(PQ::Settings {key:Some("toolbarSize".into())}).data.unwrap()["setting"]["value"],size);
        assert_eq!(window.hud.height_request(),top_bar_height(model.state().borrow().top_bar_size));
    }
    for (key,value) in [("settingsPanelPosition",serde_json::json!([30,100])),("settingsPanelSize",serde_json::json!([700,650]))] {
        assert!(apply(&pref_request(PE::Setting {key:key.into(),value:Some(value)},key)).ok);
    }
    assert_eq!(window.settings_layout.borrow().as_ref().unwrap().geometry(),(Some((30,100)),(700,650)));
    assert!(apply(&pref_request(PE::Setting {key:"settingsPanelPosition".into(),value:None},"position-reset")).ok);
    assert_eq!(window.settings_layout.borrow().as_ref().unwrap().geometry().0,None);
    let custom=LauncherSpec {id:"custom-cli".into(),name:"CLI test".into(),icon:"🤖".into(),executable:"/bin/true".into(),arguments:vec![secret.into()]};
    assert!(apply(&pref_request(PE::CustomPut {launcher:custom,create:true},"custom-add")).ok);
    assert_eq!(pref_read(PQ::Custom {id:"custom-cli".into()}).data.unwrap()["launcher"]["arguments"][0],secret);
    assert!(apply(&pref_request(PE::Visibility {keys:Some(vec!["custom-cli".into()])},"visible")).ok);
    assert_eq!(model.state().borrow().visible_harnesses,Some(vec!["custom-cli".to_string()]));
    assert!(apply(&pref_request(PE::CustomRemove {id:"custom-cli".into()},"custom-remove")).ok);
    assert!(model.state().borrow().custom_harnesses.is_empty());
    assert_eq!(model.state().borrow().visible_harnesses,Some(vec![]));
    assert_eq!(pref_read(PQ::Custom {id:"custom-cli".into()}).exit_code(),3);
    assert!(!window.window.is_visible());
    let mut shortcut=Request {control_version:1,request_id:"shortcut-state".into(),command:Command::Shortcut {combo:"SUPER + F8".into(),preview:None,expect_epoch:None,expect_revision:None}};
    let preview=window.cli_shortcut(&model,&shortcut,false).data.unwrap();
    if let Command::Shortcut {preview:p,expect_epoch,expect_revision,..}=&mut shortcut.command {*p=Some("a".repeat(64));*expect_epoch=Some(preview["epoch"].as_str().unwrap().into());*expect_revision=Some(preview["revision"].as_str().unwrap().into());}
    assert!(window.cli_shortcut(&model,&shortcut,false).ok);assert!(window.cli_shortcut(&model,&shortcut,true).ok);
    assert_eq!(model.state().borrow().toggle_shortcut.as_deref(),Some("SUPER + F8"));
    assert_eq!(window.cli_shortcut(&model,&shortcut,false).exit_code(),5,"old workspace revision must fail after shortcut state changes");
    // Exercise the owner handlers against the same live editor buffer as GTK.
    let file=root.join("example.txt");
    std::fs::write(&file,"original").unwrap();
    let opened=apply(&prepare(Edit::OpenEditor {folder:root.display().to_string(),file:Some("example.txt".into())},"open-editor"));
    assert!(opened.ok,"{opened:?}");
    let editors=read(Query::Editors).data.unwrap();
    let editor=editors["editors"][0]["id"].as_str().unwrap().to_owned();
    let document=editors["editors"][0]["documents"][0]["id"].as_str().unwrap().to_owned();
    let buffer=|| read(Query::Buffer {editor:editor.clone(),document:document.clone(),offset:0}).data.unwrap()["buffer"].clone();
    assert_eq!(buffer()["text"],"original");
    use crate::control::EditorAction;
    let edit=EditorAction::Splice {document:document.clone(),revision:buffer()["bufferRevision"].as_str().unwrap().into(),start:0,end:8,text:"updated".into()};
    let operation=|action|Edit::Editor {editor:editor.clone(),action};
    assert!(apply(&prepare(operation(edit.clone()),"change-buffer")).ok);
    assert!(!apply(&prepare(operation(edit),"stale-buffer")).ok);
    assert_eq!(std::fs::read_to_string(&file).unwrap(),"original");
    let revision=buffer()["bufferRevision"].as_str().unwrap().to_string();
    assert!(apply(&prepare(operation(EditorAction::Save {document,revision}),"save-buffer")).ok);
    assert_eq!(std::fs::read_to_string(&file).unwrap(),"updated");
    let mixed=read(Query::DesktopLayout).data.unwrap()["layout"].clone();
    assert_eq!(mixed["version"],2);
    assert!(mixed["items"].as_array().unwrap().iter().any(|i|i["kind"]=="editor" && i["id"]==editor));
    assert!(read(Query::ValidateLayout {layout:serde_json::from_value(mixed).unwrap()}).ok);
    assert!(apply(&prepare(Edit::ArrangeDesktop,"arrange-desktop")).ok);
    window.window.close(); drop(old);
    crate::state::flush_state_saves_checked().unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn icon_slots_first_minimize_fills_from_bottom_left() {
    crate::gtk_test::run_in_child_process("window::tests::icon_slots_first_minimize_inner");
}

#[test]
fn icon_slots_first_minimize_inner() {
    if !crate::gtk_test::is_child() { return; }
    let root = std::env::temp_dir().join(format!("sd-icon-slots-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for name in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMUX_TMPDIR"] {
        std::env::set_var(name, &root);
    }
    std::env::remove_var("TMUX");
    std::env::remove_var("TMUX_PANE");
    gtk4::init().unwrap();
    let app = gtk4::Application::new(Some("com.superdesktop.IconSlotsTest"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gtk4::gio::Cancellable>).unwrap();
    let card_json = |id: &str, x: i32, extra: serde_json::Value| {
        let mut value = serde_json::json!({
            "id":id, "session_name":id, "agent_type":"shell", "command":"/bin/false",
            "x":x, "y":150, "width":400, "height":300, "restored_width":400, "restored_height":300,
            "created_at":0.0
        });
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value::<TerminalData>(value).unwrap()
    };
    let mut state = AppState::default();
    state.terminals.push(card_json("sd_icon_a", 100, serde_json::json!({})));
    state.terminals.push(card_json("sd_icon_b", 300, serde_json::json!({})));
    // Minimized before, and dragged to a spot of its own.
    state.terminals.push(card_json("sd_icon_old", 500, serde_json::json!({
        "iconified":true, "width":128, "height":128, "icon_x":600, "icon_y":200
    })));
    let model = crate::workspace_model::LocalWorkspace::new(state);
    let window = SuperDesktopWindow::new(&app, || {}, Rc::new(crate::hotcorner::Zone::default()), model.state());
    let (sh, (dock_left, _)) = (window.screen_height(), crate::plugin_ui::slots::insets());
    let bottom = sh - ICON_ROW_GAP - crate::mini_terminal::ICON_SIZE;
    let icon = |id: &str| {
        let card = window.any_terminal_card(id).unwrap();
        let data = card.data.borrow();
        (data.icon_x, data.icon_y)
    };

    // The first minimize takes the bottom-left spot, the next one the spot
    // to its right; neither stays where its card stood.
    assert!(window.any_terminal_card("sd_icon_a").unwrap().set_iconified(true));
    assert_eq!(icon("sd_icon_a"), (Some(ICON_ROW_GAP + dock_left), Some(bottom)));
    assert!(window.any_terminal_card("sd_icon_b").unwrap().set_iconified(true));
    assert_eq!(icon("sd_icon_b"), (Some(ICON_ROW_GAP + dock_left + 140), Some(bottom)));

    // A moved icon keeps its spot through restore and minimize.
    let a = window.any_terminal_card("sd_icon_a").unwrap();
    set_displayed_pos(&mut a.data.borrow_mut(), 700, 300);
    assert!(a.set_iconified(false));
    assert!(a.set_iconified(true));
    assert_eq!(icon("sd_icon_a"), (Some(700), Some(300)));
    let old = window.any_terminal_card("sd_icon_old").unwrap();
    assert!(old.set_iconified(false));
    assert!(old.set_iconified(true));
    assert_eq!(icon("sd_icon_old"), (Some(600), Some(200)));

    crate::state::flush_state_saves_checked().unwrap();
    let saved = crate::state::load_state();
    let saved_icon = |id: &str| saved.terminals.iter().find(|t| t.id == id).map(|t| (t.icon_x, t.icon_y)).unwrap();
    assert_eq!(saved_icon("sd_icon_b"), (Some(ICON_ROW_GAP + dock_left + 140), Some(bottom)));
    assert_eq!(saved_icon("sd_icon_a"), (Some(700), Some(300)));
    window.window.close();
    crate::state::flush_state_saves_checked().unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn cli_mode_handler_checks_live_widgets_and_persists_layout() {
    crate::gtk_test::run_in_child_process("window::tests::cli_mode_handler_inner");
}

#[test]
fn cli_mode_handler_inner() {
    if !crate::gtk_test::is_child() { return; }
    use crate::control::{Command, ModeAction, Request};
    let root = std::env::temp_dir().join(format!("sd-mode-window-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for name in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMUX_TMPDIR"] {
        std::env::set_var(name, &root);
    }
    std::env::remove_var("TMUX");
    std::env::remove_var("TMUX_PANE");
    gtk4::init().unwrap();
    let app = gtk4::Application::new(Some("com.superdesktop.CliModeTest"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gtk4::gio::Cancellable>).unwrap();
    let mut state = AppState::default();
    state.terminals.push(serde_json::from_value(serde_json::json!({
        "id":"sd_term_mode", "session_name":"sd_term_mode", "agent_type":"shell", "command":"/bin/false",
        "x":100, "y":200, "width":128, "height":128, "restored_width":480, "restored_height":320,
        "iconified":true, "icon_x":300, "icon_y":400, "created_at":0.0
    })).unwrap());
    let model = crate::workspace_model::LocalWorkspace::new(state);
    let window = SuperDesktopWindow::new(&app, || {}, Rc::new(crate::hotcorner::Zone::default()), model.state());
    let card = window.any_terminal_card("sd_term_mode").unwrap();
    let request = |action| {
        let snapshot = window.desktop_snapshot(&model).unwrap();
        Request { control_version: 1, request_id: "mode-test".into(), command: Command::Mode {
            id: "sd_term_mode".into(), action, expect_epoch: snapshot.epoch.clone(),
            expect_revision: crate::control_geometry::revision(&snapshot, &snapshot.cards[0]),
        }}
    };
    let restore = request(ModeAction::Restore);
    card.container.add_css_class("term-resizing");
    assert_eq!(window.cli_geometry(&model, &restore).exit_code(), 5);
    card.container.remove_css_class("term-resizing");
    let reply = window.cli_geometry(&model, &restore);
    assert!(reply.ok, "{reply:?}");
    assert_eq!(reply.data.as_ref().unwrap()["mode"], "normal");
    assert_eq!(reply.data.as_ref().unwrap()["changed"], true);
    assert_eq!(model.state().borrow().terminals[0].width, 480);
    assert_eq!(window.cli_geometry(&model, &restore).exit_code(), 5, "old revision must conflict");
    let noop = window.cli_geometry(&model, &request(ModeAction::Restore));
    assert_eq!(noop.data.unwrap()["changed"], false);
    for (action, mode) in [(ModeAction::Minimize, "minimized"), (ModeAction::Expand, "expanded"), (ModeAction::Collapse, "minimized")] {
        let reply = window.cli_geometry(&model, &request(action));
        assert!(reply.ok, "{reply:?}");
        let data = reply.data.unwrap();
        assert_eq!(data["mode"], mode);
        assert_eq!(data["changed"], true);
    }
    let action=|action| {let snapshot=window.desktop_snapshot(&model).unwrap();Request {control_version:1,request_id:"card-action".into(),command:Command::CardAction {id:"sd_term_mode".into(),action,expect_epoch:snapshot.epoch.clone(),expect_revision:crate::control_geometry::revision(&snapshot,&snapshot.cards[0])}}};
    let tagged=window.cli_geometry(&model,&action(crate::control::CardAction::Tag {value:7}));assert!(tagged.ok,"{tagged:?}");
    assert_eq!(model.state().borrow().terminals[0].tag,7);
    assert!(window.cli_geometry(&model,&action(crate::control::CardAction::Raise)).ok);
    assert_eq!(window.cli_geometry(&model,&action(crate::control::CardAction::Focus)).exit_code(),6,"hidden workspace must not take focus");
    assert!(!window.window.is_visible());
    crate::state::flush_state_saves_checked().unwrap();
    let persisted = crate::state::load_state();
    assert!(persisted.terminals[0].iconified);
    assert_eq!(persisted.terminals[0].restored_width, 480);
    assert_eq!(persisted.terminals[0].icon_x, Some(300));
    window.window.close();
    crate::state::flush_state_saves_checked().unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn test_card_slide_goes_to_the_nearest_left_or_right_edge() {
    let sw = 2560.0;
    // Top-left corner → left, Y unchanged.
    let (sx, sy) = card_slide_offscreen(80.0, 40.0, 128.0, sw);
    assert!(sx < 0.0, "left-half card must leave to the left, got {sx}");
    assert_eq!(sy, 40.0);
    // Bottom-right corner → right, Y unchanged.
    let (sx, sy) = card_slide_offscreen(2200.0, 1400.0, 380.0, sw);
    assert!(sx > sw, "right-half card must leave to the right, got {sx}");
    assert_eq!(sy, 1400.0);
    // Top-right icon → right, not up.
    let (sx, sy) = card_slide_offscreen(2400.0, 20.0, 128.0, sw);
    assert!(sx > sw);
    assert_eq!(sy, 20.0);
}

#[test]
fn hide_fallback_does_not_wait_on_a_stuck_compositor() {
    // The fallback must fire after the cards are visually gone, and well
    // before a frozen overlay feels like a hang.
    assert!(HIDE_FALLBACK >= Duration::from_millis(120));
    assert!(HIDE_FALLBACK <= Duration::from_millis(400));
    let (x, _) = slide_pose_at(1.0, -SLIDE_LAUNCH_OUT, 0.0, SLIDE_OMEGA_OUT, HIDE_FALLBACK);
    assert!(x < 0.01, "the fallback unmaps cards still {:.1}% on screen", x * 100.0);
}

/// Where a slide starting at (`x`, `v`) is after `elapsed` of 60 Hz frames.
fn slide_pose_at(x: f64, v: f64, target: f64, omega: f64, elapsed: Duration) -> (f64, f64) {
    let frames = (elapsed.as_secs_f64() * 60.0).floor() as usize;
    (0..frames).fold((x, v), |(x, v), _| spring_step(x, v, target, 1.0 / 60.0, omega))
}

#[test]
fn a_hiding_overlay_cannot_take_the_keyboard_back() {
    // Released: the focus/pointer leave handlers the release itself
    // triggers must not flip the surface back to OnDemand/Exclusive.
    assert!(overlay_keyboard_mode_allowed(KeyboardMode::None, true));
    assert!(!overlay_keyboard_mode_allowed(KeyboardMode::OnDemand, true));
    assert!(!overlay_keyboard_mode_allowed(KeyboardMode::Exclusive, true));
    for mode in [KeyboardMode::None, KeyboardMode::OnDemand, KeyboardMode::Exclusive] {
        assert!(overlay_keyboard_mode_allowed(mode, false));
    }
}

#[test]
fn hide_slides_cards_from_where_they_are_now() {
    crate::gtk_test::run_in_child_process("window::tests::hide_slides_cards_from_where_they_are_now_inner");
}

#[test]
fn hide_slides_cards_from_where_they_are_now_inner() {
    if !crate::gtk_test::is_child() { return; }
    let root = std::env::temp_dir().join(format!("sd-slide-out-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for name in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMUX_TMPDIR"] {
        std::env::set_var(name, &root);
    }
    std::env::remove_var("TMUX");
    std::env::remove_var("TMUX_PANE");
    gtk4::init().unwrap();
    let app = gtk4::Application::new(Some("com.superdesktop.SlideOutTest"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gtk4::gio::Cancellable>).unwrap();
    let mut state = AppState::default();
    state.terminals.push(serde_json::from_value(serde_json::json!({
        "id":"sd_term_slide", "session_name":"sd_term_slide", "agent_type":"shell", "command":"/bin/false",
        "x":20, "y":200, "width":160, "height":120, "created_at":0.0
    })).unwrap());
    let model = crate::workspace_model::LocalWorkspace::new(state);
    let window = SuperDesktopWindow::new(&app, || {}, Rc::new(crate::hotcorner::Zone::default()), model.state());
    let card = window.any_terminal_card("sd_term_slide").unwrap();
    let widget: gtk4::Widget = card.container.clone().upcast();
    let sw = window.screen_width() as f64;
    let path = || *window.anim_trajectories.borrow().get(&widget).unwrap();

    window.start_slide_in();
    assert!(path().sx < 0.0, "a left-half card enters from the left");
    // The slide-in settles; the user then drags the card to the right.
    window.slide.running.set(false);
    window.slide.gen.set(window.slide.gen.get().wrapping_add(1));
    let right = (sw - 200.0) as i32;
    card.data.borrow_mut().x = right;

    window.start_slide_out(|| {});
    let out = path();
    assert_eq!((out.tx, out.ty), (right as f64, 200.0), "hide must start where the card is");
    assert!(out.sx > sw, "a right-half card must leave to the right, got {}", out.sx);

    // Reversing a hide mid-flight keeps the same path, so nothing jumps.
    card.data.borrow_mut().x = 20;
    window.start_slide_in();
    assert_eq!(path(), out);
    window.window.close();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn settings_survives_overlay_toggle_until_closed() {
    crate::gtk_test::run_in_child_process("window::tests::settings_survives_overlay_toggle_inner");
}

#[test]
fn settings_survives_overlay_toggle_inner() {
    if !crate::gtk_test::is_child() { return; }
    let root = std::env::temp_dir().join(format!("sd-settings-toggle-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for name in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"] {
        std::env::set_var(name, &root);
    }
    gtk4::init().unwrap();
    let app = gtk4::Application::new(Some("com.superdesktop.SettingsToggleTest"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gtk4::gio::Cancellable>).unwrap();
    let state = AppState { welcome_tour_seen: true, ..AppState::default() };
    let model = crate::workspace_model::LocalWorkspace::new(state);
    let window = SuperDesktopWindow::new(&app, || {}, Rc::new(crate::hotcorner::Zone::default()), model.state());
    let tree = window.window.upcast_ref::<gtk4::Widget>();
    window.show_again();
    crate::gtk_test::find_first::<Button>(tree, "hud-gear").unwrap().emit_clicked();
    crate::gtk_test::find_first::<Button>(tree, "settings-top-bar-entry").unwrap().emit_clicked();
    let size = crate::gtk_test::find_first::<Button>(tree, "top-bar-size").unwrap();
    assert!(size.is_mapped());

    window.start_slide_out(|| {});
    window.hide_now();
    window.show_again();
    assert!(size.is_mapped(), "the selected Settings page must return with the overlay");

    let panel = crate::gtk_test::find_first::<gtk4::Box>(tree, "harness-panel").unwrap();
    let close = crate::gtk_test::find_where(panel.upcast_ref(), |w| w.tooltip_text().as_deref() == Some("Close panel"))
        .unwrap().downcast::<Button>().unwrap();
    close.emit_clicked();
    window.hide_now();
    window.show_again();
    assert!(!size.is_mapped(), "explicitly closed Settings must stay closed");
    window.window.close();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn renderer_warm_up_leaves_the_overlay_unmapped() {
    crate::gtk_test::run_in_child_process("window::tests::renderer_warm_up_leaves_the_overlay_unmapped_inner");
}

#[test]
fn renderer_warm_up_leaves_the_overlay_unmapped_inner() {
    if !crate::gtk_test::is_child() { return; }
    let root = std::env::temp_dir().join(format!("sd-warm-up-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for name in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMUX_TMPDIR"] {
        std::env::set_var(name, &root);
    }
    std::env::remove_var("TMUX");
    std::env::remove_var("TMUX_PANE");
    gtk4::init().unwrap();
    let app = gtk4::Application::new(Some("com.superdesktop.WarmUpTest"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gtk4::gio::Cancellable>).unwrap();
    let model = crate::workspace_model::LocalWorkspace::new(AppState::default());
    let window = SuperDesktopWindow::new(&app, || {}, Rc::new(crate::hotcorner::Zone::default()), model.state());

    window.prewarm_renderer();
    assert!(window.window.is_realized(), "the warm-up needs the window's renderer");
    assert!(window.window.renderer().is_some());
    assert!(!window.window.is_mapped(), "the warm-up must never put the overlay on screen");
    assert!(!window.window.is_visible());
    // Harmless when repeated (a second warm-up, or one after a show).
    window.prewarm_renderer();
    assert!(!window.window.is_mapped());
    window.window.close();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn show_slides_cards_in_from_the_edges_of_the_real_size() {
    crate::gtk_test::run_in_child_process("window::tests::show_slides_cards_in_from_the_edges_of_the_real_size_inner");
}

#[test]
fn show_slides_cards_in_from_the_edges_of_the_real_size_inner() {
    if !crate::gtk_test::is_child() { return; }
    let root = std::env::temp_dir().join(format!("sd-slide-in-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for name in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMUX_TMPDIR"] {
        std::env::set_var(name, &root);
    }
    std::env::remove_var("TMUX");
    std::env::remove_var("TMUX_PANE");
    gtk4::init().unwrap();
    let app = gtk4::Application::new(Some("com.superdesktop.SlideInTest"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gtk4::gio::Cancellable>).unwrap();
    // Right of the middle of the 900px screen the overlay opens on, left of
    // the middle of the wider monitor the paths are first built for.
    let mut state = AppState::default();
    state.terminals.push(serde_json::from_value(serde_json::json!({
        "id":"sd_term_slide_in", "session_name":"sd_term_slide_in", "agent_type":"shell", "command":"/bin/false",
        "x":380, "y":200, "width":160, "height":120, "created_at":0.0
    })).unwrap());
    let model = crate::workspace_model::LocalWorkspace::new(state);
    let window = SuperDesktopWindow::new(&app, || {}, Rc::new(crate::hotcorner::Zone::default()), model.state());
    let card = window.any_terminal_card("sd_term_slide_in").unwrap();
    let widget: gtk4::Widget = card.container.clone().upcast();
    let path = || *window.anim_trajectories.borrow().get(&widget).unwrap();
    let monitors = gdk::Display::default().unwrap().monitors();
    let widest = (0..monitors.n_items())
        .filter_map(|i| monitors.item(i).and_then(|m| m.downcast::<gdk::Monitor>().ok()))
        .map(|m| m.geometry().width())
        .max()
        .unwrap();
    assert!(widest > 2 * 460, "the test needs a monitor wider than 920px, got {widest}");

    window.window.set_default_size(900, 600);
    window.window.present();
    // Shown again, the window has no size until its first frame is laid
    // out: the first pose must be off screen on every monitor.
    assert_eq!(allocated_size(&window.window), None);
    window.start_slide_in();
    let (sw, _) = window.slide.built_for.get();
    assert!(sw >= widest, "provisional paths must cover the widest monitor, built for {sw} < {widest}");
    let first = path();
    assert!(first.sx < 0.0 || first.sx >= f64::from(widest), "first pose on screen: {}", first.sx);

    // Broadway ticks slowly; the rebuild, not the settle, is what counts.
    let rebuilt = || allocated_size(&window.window).is_some_and(|size| size == window.slide.built_for.get());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !rebuilt() && std::time::Instant::now() < deadline {
        while glib::MainContext::default().iteration(false) {}
        std::thread::sleep(Duration::from_millis(5));
    }
    let size = allocated_size(&window.window).expect("the window was laid out");
    assert_eq!(size.0, 900, "the window must take its requested width");
    assert_eq!(window.slide.built_for.get(), size, "the paths must be rebuilt for the real size");
    let entry = path();
    assert_eq!(entry.sx, 940.0, "a right-half card must enter from the real right edge");
    assert_eq!((entry.tx, entry.ty), (380.0, 200.0));
    window.window.close();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn cards_follow_the_display_size_and_expand_to_it() {
    crate::gtk_test::run_in_child_process("window::tests::cards_follow_the_display_size_and_expand_to_it_inner");
}

#[test]
fn cards_follow_the_display_size_and_expand_to_it_inner() {
    if !crate::gtk_test::is_child() { return; }
    let root = std::env::temp_dir().join(format!("sd-follow-display-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for name in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMUX_TMPDIR"] {
        std::env::set_var(name, &root);
    }
    std::env::remove_var("TMUX");
    std::env::remove_var("TMUX_PANE");
    gtk4::init().unwrap();
    let app = gtk4::Application::new(Some("com.superdesktop.FollowDisplayTest"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gtk4::gio::Cancellable>).unwrap();
    // Laid out on a 3440×1440 display: a card in its bottom-right corner,
    // a minimized card in the middle with its icon in that corner, and a
    // note in the same corner.
    let mut state = AppState::default();
    state.layout_size = Some((3440, 1440));
    state.notes = vec![serde_json::from_value(serde_json::json!({
        "id":"note_corner", "text":"", "x":3130, "y":1200, "width":300, "height":230,
        "color":"omarchy", "updated_at":0.0
    })).unwrap()];
    for card in [
        serde_json::json!({"id":"sd_term_corner", "session_name":"sd_term_corner", "agent_type":"shell",
            "command":"/bin/false", "x":2790, "y":950, "width":640, "height":480, "created_at":0.0}),
        serde_json::json!({"id":"sd_term_icon", "session_name":"sd_term_icon", "agent_type":"shell",
            "command":"/bin/false", "x":1400, "y":510, "width":128, "height":128,
            "restored_width":640, "restored_height":480, "iconified":true,
            "icon_x":3302, "icon_y":1302, "created_at":0.0}),
    ] {
        state.terminals.push(serde_json::from_value(card).unwrap());
    }
    let model = crate::workspace_model::LocalWorkspace::new(state);
    let window = SuperDesktopWindow::new(&app, || {}, Rc::new(crate::hotcorner::Zone::default()), model.state());
    let corner = window.any_terminal_card("sd_term_corner").unwrap();
    let icon = window.any_terminal_card("sd_term_icon").unwrap();
    let note = Rc::clone(&window.note_cards.borrow()[0]);
    let at = |widget: &gtk4::Widget| window.canvas.child_position(widget);
    let pump_until = |done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !done() && std::time::Instant::now() < deadline {
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    // `child_position` reports a moved card once the canvas laid out again.
    let assert_at = |widget: &gtk4::Widget, spot: (f64, f64)| {
        pump_until(&|| at(widget) == spot);
        assert_eq!(at(widget), spot);
    };
    let show = |size: (i32, i32)| {
        window.window.set_default_size(size.0, size.1);
        window.window.present();
        pump_until(&|| window.state.borrow().layout_size == Some(size));
        assert_eq!(allocated_size(&window.window), Some(size), "the window must take the size");
        assert_eq!(window.state.borrow().layout_size, Some(size), "the layout must follow the size");
    };

    // The 900×600 display, shown as the shortcut does: the slide starts
    // before the overlay has its size, and takes every card to its new
    // place, its corner on this display, whole on screen.
    window.start_slide_in();
    show((900, 600));
    pump_until(&|| window.slide.built_for.get() == (900, 600));
    let target = |widget: &gtk4::Widget| {
        let path = *window.anim_trajectories.borrow().get(widget).unwrap();
        (path.tx, path.ty)
    };
    assert_eq!(target(corner.container.upcast_ref()), (250.0, 110.0), "the slide must end at the new place");
    assert_eq!(target(icon.container.upcast_ref()), (762.0, 462.0));
    assert_eq!(target(note.container.upcast_ref()), (590.0, 360.0));
    // Broadway ticks too slowly to wait for the settle: stop the tick and
    // put every card where the slide ends.
    window.slide.gen.set(window.slide.gen.get().wrapping_add(1));
    window.slide.running.set(false);
    window.paint_slide(1.0);
    let d = corner.data.borrow().clone();
    assert_eq!((d.x, d.y, d.width, d.height), (250, 110, 640, 480));
    assert_at(corner.container.upcast_ref(), (250.0, 110.0));
    let d = icon.data.borrow().clone();
    assert_eq!((d.x, d.y, d.icon_x, d.icon_y), (130, 90, Some(762), Some(462)));
    assert_eq!((d.restored_width, d.restored_height), (640, 480));
    assert_at(icon.container.upcast_ref(), (762.0, 462.0));
    assert_eq!((note.data.borrow().x, note.data.borrow().y), (590, 360));
    assert_at(note.container.upcast_ref(), (590.0, 360.0));
    let saved = window.state.borrow().terminals.iter().find(|t| t.id == "sd_term_corner").cloned().unwrap();
    assert_eq!((saved.x, saved.y), (250, 110), "the new places are saved");

    // Expand fills 80% of this display, not of the one the card was made on.
    corner.press_expand_button();
    assert!(corner.is_expanded());
    assert_at(corner.container.upcast_ref(), (90.0, 60.0));
    assert_eq!(corner.container.size_request(), (720, 480));

    // A larger display: the expanded card grows with it, the card's own
    // place moves with the rest.
    show((1000, 700));
    assert!(corner.is_expanded());
    assert_at(corner.container.upcast_ref(), (100.0, 70.0));
    assert_eq!(corner.container.size_request(), (800, 560));
    assert_eq!((corner.data.borrow().x, corner.data.borrow().y), (350, 210));
    assert_at(icon.container.upcast_ref(), (862.0, 562.0));
    assert_at(note.container.upcast_ref(), (690.0, 460.0));
    window.window.close();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn slide_takes_plugin_cards_docks_panels_and_full_screen_along() {
    crate::gtk_test::run_in_child_process("window::tests::slide_takes_plugins_along_inner");
}

/// Hiding and showing slides a plugin's card, dock, panel and full-screen
/// surface out and in with the terminal cards: off the nearer left or
/// right edge, and back to where they were.
#[test]
fn slide_takes_plugins_along_inner() {
    if !crate::gtk_test::is_child() { return; }
    let root = std::env::temp_dir().join(format!("sd-slide-plugins-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for name in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMUX_TMPDIR"] {
        std::env::set_var(name, &root);
    }
    std::env::set_var("SD_PLUGINS_DIR", root.join("plugins"));
    std::env::set_var("SD_PLUGINS_STATE_DIR", root.join("state"));
    std::env::remove_var("TMUX");
    std::env::remove_var("TMUX_PANE");
    let kit = root.join("plugins/kit");
    std::fs::create_dir_all(&kit).unwrap();
    std::fs::write(kit.join("plugin.json"), serde_json::json!({"id": "kit", "api": 1, "surfaces": [
        {"id": "side", "slot": "dock.right", "qml": "Part.qml", "size": 64},
        {"id": "board", "slot": "card", "qml": "Part.qml", "size": 300, "height": 200},
        {"id": "win", "slot": "panel", "qml": "Part.qml", "size": 240, "height": 160},
        {"id": "full", "slot": "fullscreen", "qml": "Part.qml"}
    ]}).to_string()).unwrap();
    std::fs::write(kit.join("Part.qml"), "import QtQuick\nRectangle { color: \"teal\" }\n").unwrap();
    gtk4::init().unwrap();
    let app = gtk4::Application::new(Some("com.superdesktop.SlidePluginsTest"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gtk4::gio::Cancellable>).unwrap();
    let mut state = AppState::default();
    state.layout_size = Some((900, 600));
    state.terminals.push(serde_json::from_value(serde_json::json!({"id":"sd_term_a", "session_name":"sd_term_a",
        "agent_type":"shell", "command":"/bin/false", "x":100, "y":100, "width":400, "height":300, "created_at":0.0})).unwrap());
    let model = crate::workspace_model::LocalWorkspace::new(state);
    let window = SuperDesktopWindow::new(&app, || {}, Rc::new(crate::hotcorner::Zone::default()), model.state());
    window.window.set_default_size(900, 600);
    window.window.present();
    let pump_until = |what: &str, done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !done() && std::time::Instant::now() < deadline {
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(done(), "timed out: {what}");
    };
    pump_until("the window's size", &|| allocated_size(&window.window) == Some((900, 600)));
    window.slide.gen.set(window.slide.gen.get().wrapping_add(1));
    window.slide.running.set(false);
    window.paint_slide(1.0);
    let slots = crate::plugin_ui::slots::get().expect("plugin slots");
    slots.open_card("kit", "board", "").unwrap();
    slots.open_panel("kit", "win").unwrap();
    let root_widget = window.window.clone().upcast::<gtk4::Widget>();
    let one = |class: &str| crate::gtk_test::find_all::<gtk4::Widget>(&root_widget, class).pop().unwrap_or_else(|| panic!("no .{class}"));
    let terminal = window.any_terminal_card("sd_term_a").unwrap().container.clone().upcast::<gtk4::Widget>();
    // The plugin's panel, not another kind waiting hidden.
    let panel = crate::gtk_test::find_all::<gtk4::Widget>(&root_widget, "harness-panel").into_iter()
        .find(|p| !crate::gtk_test::find_all::<gtk4::Widget>(p, "plugin-surface").is_empty())
        .expect("the plugin's panel");
    let parts = [("terminal", terminal), ("plugin card", one("plugin-card")), ("dock", one("plugin-dock-edge")), ("panel", panel)];
    let x = |widget: &gtk4::Widget| widget.compute_bounds(&window.window).map(|b| (b.x().round() as i32, b.width().round() as i32));
    pump_until("everything laid out", &|| parts.iter().all(|(_, w)| x(w).is_some_and(|(_, width)| width > 0)));
    // Where they rest once the layout stops changing: a surface whose QML
    // host is missing shows its error a moment later, at another size.
    let mut rest: Vec<(i32, i32)> = Vec::new();
    for _ in 0..60 {
        for _ in 0..10 {
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(Duration::from_millis(5));
        }
        let now: Vec<(i32, i32)> = parts.iter().map(|(_, w)| x(w).unwrap()).collect();
        if now == rest { break; }
        rest = now;
    }
    assert!(rest[2].0 >= 900 - 64 - 16, "the dock is at the right edge: {:?}", rest[2]);
    // A terminal's path uses its saved width, which Broadway's fonts
    // make narrower than drawn: it is gone once it is past the edge.
    let gone = |(name, w): &(&str, gtk4::Widget)| x(w).is_some_and(|(x, width)| {
        x >= 900 || if *name == "terminal" { x < 0 } else { x + width <= 0 }
    });

    // Hidden: every one of them off screen, by its nearer edge.
    window.ensure_slide_trajectories(true);
    window.paint_slide(0.0);
    pump_until("all off screen", &|| parts.iter().all(gone));
    for ((name, widget), (rest_x, width)) in parts.iter().zip(&rest) {
        let (now, _) = x(widget).unwrap();
        let right = rest_x + width / 2 >= 450;
        assert_eq!(now >= 900, right, "{name} left by the wrong edge: rest {rest_x}, now {now}");
    }
    // Halfway, they are on their way.
    window.paint_slide(0.5);
    pump_until("half way", &|| parts.iter().zip(&rest).all(|((_, w), r)| x(w).is_some_and(|now| now.0 != r.0 && now.0 < 900 && now.0 + now.1 > 0)));
    // Shown: all back where they were.
    window.paint_slide(1.0);
    // Back at the same x; a terminal's width follows its status text.
    let back = || parts.iter().zip(&rest).all(|((_, w), r)| x(w).is_some_and(|now| now.0 == r.0));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !back() && std::time::Instant::now() < deadline {
        while glib::MainContext::default().iteration(false) {}
        std::thread::sleep(Duration::from_millis(5));
    }
    let now: Vec<(&str, Option<(i32, i32)>)> = parts.iter().map(|(name, w)| (*name, x(w))).collect();
    assert!(back(), "not back where they were: rest {rest:?}, now {now:?}");

    // A full-screen surface leaves by the right edge and comes back.
    slots.open_fullscreen("kit", "full").unwrap();
    let full = one("plugin-fullscreen");
    pump_until("full screen laid out", &|| x(&full) == Some((0, 900)));
    window.paint_slide(0.0);
    pump_until("full screen off", &|| x(&full).is_some_and(|(x, _)| x >= 900));
    window.paint_slide(1.0);
    pump_until("full screen back", &|| x(&full) == Some((0, 900)));
    slots.close_fullscreen();
    window.window.close();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn top_bar_editor_button_opens_sd_editor() {
    crate::gtk_test::run_in_child_process("window::tests::top_bar_editor_button_inner");
}

/// ✎ in the top bar, after 📝: SD Editor on the top bar's folder. Turned
/// off in Settings it is not there.
#[test]
fn top_bar_editor_button_inner() {
    if !crate::gtk_test::is_child() { return; }
    let root = std::env::temp_dir().join(format!("sd-editor-button-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    for name in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMUX_TMPDIR"] {
        std::env::set_var(name, &root);
    }
    std::env::remove_var("TMUX");
    std::env::remove_var("TMUX_PANE");
    gtk4::init().unwrap();
    // The repository's icons, as an install puts them on the search path.
    gtk4::IconTheme::for_display(&gtk4::gdk::Display::default().unwrap())
        .add_search_path(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/icons"));
    let app = gtk4::Application::new(Some("com.superdesktop.EditorButtonTest"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gtk4::gio::Cancellable>).unwrap();
    let mut state = AppState::default();
    state.workspace_dir = Some(project.to_string_lossy().into_owned());
    let model = crate::workspace_model::LocalWorkspace::new(state);
    let window = SuperDesktopWindow::new(&app, || {}, Rc::new(crate::hotcorner::Zone::default()), model.state());
    let hud = window.hud.clone().upcast::<gtk4::Widget>();
    let button = crate::gtk_test::find_all::<gtk4::Button>(&hud, "sd-editor-button").pop().expect("✎ in the top bar");
    assert!(button.get_visible());
    // The bolt-and-pencil icon (the repository's icons are on the search path).
    assert_eq!(button.child().and_downcast::<gtk4::Image>().and_then(|i| i.icon_name()).as_deref(), Some(crate::editor::ICON));
    // A picture of the bar: SD_EDITOR_SHOTS=<dir> cargo test top_bar_editor_button.
    if let Some(dir) = std::env::var_os("SD_EDITOR_SHOTS") {
        window.window.set_default_size(1000, 400);
        window.window.present();
        crate::gtk_test::pump(400);
        crate::gtk_test::save_png(&button.parent().unwrap(), &std::path::PathBuf::from(dir).join("top-bar.png"));
    }
    // Right after 📝 +.
    let previous = button.prev_sibling().and_downcast::<gtk4::Button>().expect("a button before ✎");
    assert_eq!(previous.label().as_deref(), Some("📝 +"));
    button.emit_clicked();
    let cards = crate::editor::cards();
    assert_eq!(cards.len(), 1, "SD Editor opened");
    assert!(cards[0].container.parent().is_some(), "on the canvas");
    crate::editor::set_button_shown(false);
    assert!(!button.get_visible());
    window.window.close();

    // Turned off in Settings: not shown after a restart either.
    let mut state = AppState::default();
    state.editor_button_hidden = true;
    let model = crate::workspace_model::LocalWorkspace::new(state);
    let window = SuperDesktopWindow::new(&app, || {}, Rc::new(crate::hotcorner::Zone::default()), model.state());
    let hud = window.hud.clone().upcast::<gtk4::Widget>();
    let button = crate::gtk_test::find_all::<gtk4::Button>(&hud, "sd-editor-button").pop().unwrap();
    assert!(!button.get_visible());
    window.window.close();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn plugins_card_commands_change_cards_like_their_buttons() {
    crate::gtk_test::run_in_child_process("window::tests::plugin_card_commands_inner");
}

/// `cards.*` from a plugin move, size, minimize, expand, tag and close a
/// terminal card the way its buttons and edges do, inside the area cards
/// may cover, and closing asks the user on the card first.
#[test]
fn plugin_card_commands_inner() {
    if !crate::gtk_test::is_child() { return; }
    use crate::plugin_ui::api::CardCommand as C;
    let root = std::env::temp_dir().join(format!("sd-plugin-card-commands-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for name in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMUX_TMPDIR"] {
        std::env::set_var(name, &root);
    }
    std::env::remove_var("TMUX");
    std::env::remove_var("TMUX_PANE");
    gtk4::init().unwrap();
    let app = gtk4::Application::new(Some("com.superdesktop.PluginCardCommandsTest"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gtk4::gio::Cancellable>).unwrap();
    let mut state = AppState::default();
    state.layout_size = Some((900, 600));
    state.notes = vec![serde_json::from_value(serde_json::json!({
        "id":"note_a", "text":"", "x":600, "y":100, "width":200, "height":150, "color":"omarchy", "updated_at":0.0
    })).unwrap()];
    state.terminals.push(serde_json::from_value(serde_json::json!({"id":"sd_term_a", "session_name":"sd_term_a",
        "agent_type":"shell", "command":"/bin/false", "x":100, "y":100, "width":400, "height":300, "created_at":0.0})).unwrap());
    let model = crate::workspace_model::LocalWorkspace::new(state);
    let window = SuperDesktopWindow::new(&app, || {}, Rc::new(crate::hotcorner::Zone::default()), model.state());
    window.window.set_default_size(900, 600);
    window.window.present();
    let pump_until = |what: &str, done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !done() && std::time::Instant::now() < deadline {
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(done(), "timed out: {what}");
    };
    pump_until("the window's size", &|| allocated_size(&window.window) == Some((900, 600)));
    // Shown without the slide: the cards are where they belong.
    window.slide.gen.set(window.slide.gen.get().wrapping_add(1));
    window.slide.running.set(false);
    let card = window.any_terminal_card("sd_term_a").unwrap();
    let answer: Rc<RefCell<Option<Result<serde_json::Value, serde_json::Value>>>> = Rc::new(RefCell::new(None));
    let ask = |id: &str, command: C| {
        let slot = Rc::clone(&answer);
        window.plugin_card_command(id, command, "Tiles", Box::new(move |r| *slot.borrow_mut() = Some(r)));
        answer.borrow_mut().take().expect("answered at once")
    };
    let ok = |command: C| ask("sd_term_a", command).unwrap_or_else(|e| panic!("{e}"));
    let code = |command: C| ask("sd_term_a", command).unwrap_err()["code"].clone();
    let rect = |g: &serde_json::Value| (g["x"].as_i64().unwrap(), g["y"].as_i64().unwrap(), g["width"].as_i64().unwrap(), g["height"].as_i64().unwrap());

    // workspace.folder: the top bar's folder, where new agent cards start.
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    window.state.borrow_mut().workspace_dir = Some(project.to_string_lossy().into_owned());
    window.state.borrow_mut().recent_dirs = vec![project.to_string_lossy().into_owned()];
    let spec = crate::plugin_ui::surface::SurfaceSpec {
        plugin_id: "tiles".into(), plugin_name: "Tiles".into(), surface_id: "bar".into(), instance: String::new(),
        qml: root.join("Bar.qml"), dir: root.clone(),
    };
    let folder = crate::plugin_ui::api::call(&spec, "workspace.folder", &serde_json::json!({})).unwrap();
    assert_eq!(folder, serde_json::json!({"folder": project, "recent": [project]}));

    let g = ok(C::Geometry);
    assert_eq!(g["mode"], "normal");
    assert_eq!(rect(&g), (100, 100, 400, 300));
    assert_eq!(g["area"], serde_json::json!({"x": 10, "y": 70, "width": 880, "height": 520}));
    assert_eq!((g["minWidth"].as_i64(), g["minHeight"].as_i64()), (Some(320), Some(180)));
    // Kept inside the area, saved like the end of a drag.
    assert_eq!(rect(&ok(C::Move { x: 2000, y: -5 })), (490, 70, 400, 300));
    assert_eq!((card.data.borrow().x, card.data.borrow().y), (490, 70));
    let saved = || window.state.borrow().terminals.iter().find(|t| t.id == "sd_term_a").cloned().unwrap();
    assert_eq!((saved().x, saved().y), (490, 70));
    // Too big and too flat: as big as the area, as low as allowed, moved in.
    assert_eq!(rect(&ok(C::Resize { width: 5000, height: 10 })), (10, 70, 880, 180));
    assert_eq!(card.container.size_request(), (880, 180));
    assert_eq!(rect(&ok(C::Resize { width: 440, height: 520 })), (10, 70, 440, 520), "the left half");

    let g = ok(C::Minimize);
    assert_eq!(g["mode"], "minimized");
    assert_eq!(code(C::Resize { width: 500, height: 400 }), "bad_params");
    assert_eq!((rect(&ok(C::Move { x: 0, y: 0 })).0, rect(&ok(C::Move { x: 0, y: 0 })).1), (10, 70));
    assert_eq!(ok(C::Restore)["mode"], "normal");
    assert_eq!(card.data.borrow().width, 440);
    assert_eq!(ok(C::Expand)["mode"], "expanded");
    assert!(card.is_expanded());
    assert_eq!(code(C::Move { x: 50, y: 80 }), "bad_params");
    assert_eq!(ok(C::Collapse)["mode"], "normal");
    assert!(!card.is_expanded());
    // Minimizing an expanded card collapses it first.
    ok(C::Expand);
    assert_eq!(ok(C::Minimize)["mode"], "minimized");
    assert!(!card.is_expanded());
    ok(C::Restore);
    ok(C::Tag(3));
    assert_eq!(card.data.borrow().tag, 3);
    assert_eq!(saved().tag, 3);
    ok(C::Raise);
    assert_eq!(ask("note_a", C::Raise).unwrap_err()["message"], "\"note_a\" is a note");
    assert_eq!(ask("sd_term_gone", C::Raise).unwrap_err()["code"], "bad_params");

    // Closing asks on the card. A no keeps it, a yes closes it.
    pump_until("the card's ✕ on screen", &|| card.close_button().is_mapped());
    let close = |pick: &str| {
        let slot = Rc::clone(&answer);
        window.plugin_card_command("sd_term_a", C::Close, "Tiles", Box::new(move |r| *slot.borrow_mut() = Some(r)));
        assert!(answer.borrow().is_none(), "it waits for the user");
        let popover = std::iter::successors(card.close_button().first_child(), |w| w.next_sibling())
            .find_map(|w| w.downcast::<gtk4::Popover>().ok())
            .expect("the question on the card's ✕");
        let question = crate::gtk_test::find_all::<gtk4::Widget>(popover.upcast_ref(), "card-close-confirm-box").pop().unwrap();
        let label = question.first_child().unwrap().downcast::<gtk4::Label>().unwrap();
        assert!(label.label().starts_with("Tiles wants to close this card."), "{}", label.label());
        // A second request while it asks is refused.
        let second = Rc::new(RefCell::new(None));
        let slot2 = Rc::clone(&second);
        window.plugin_card_command("sd_term_a", C::Close, "Tiles", Box::new(move |r| *slot2.borrow_mut() = Some(r)));
        assert_eq!(second.borrow().as_ref().unwrap().as_ref().unwrap_err()["code"], "busy");
        let button = crate::gtk_test::descendants(popover.upcast_ref()).into_iter()
            .filter_map(|w| w.downcast::<gtk4::Button>().ok())
            .find(|b| b.label().as_deref() == Some(pick)).unwrap();
        button.emit_clicked();
        let result = answer.borrow_mut().take().expect("answered").unwrap();
        pump_until("the question to go away", &|| !std::iter::successors(card.close_button().first_child(), |w| w.next_sibling()).any(|w| w.is::<gtk4::Popover>()));
        result
    };
    assert_eq!(close("Cancel"), serde_json::json!({"closed": false}));
    assert!(window.any_terminal_card("sd_term_a").is_ok(), "a no keeps the card");
    assert_eq!(close("Close"), serde_json::json!({"closed": true}));
    assert!(window.any_terminal_card("sd_term_a").is_err(), "a yes closes it");
    assert!(window.state.borrow().terminals.iter().all(|t| t.id != "sd_term_a"));
    window.window.close();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn toolbar_launcher_order_is_restored_and_saved() {
    crate::gtk_test::run_in_child_process("window::tests::toolbar_launcher_order_inner");
}

#[test]
fn toolbar_launcher_order_inner() {
    if !crate::gtk_test::is_child() { return; }
    let root = std::env::temp_dir().join(format!("sd-launcher-order-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for name in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMUX_TMPDIR"] {
        std::env::set_var(name, &root);
    }
    std::env::remove_var("TMUX");
    std::env::remove_var("TMUX_PANE");
    gtk4::init().unwrap();
    let app = gtk4::Application::new(Some("com.superdesktop.LauncherOrderTest"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gtk4::gio::Cancellable>).unwrap();
    let mut state = AppState::default();
    state.launcher_order = vec!["shell".into(), "codex".into(), "claude".into()];
    // Shown on any machine, like the shell.
    state.custom_harnesses = vec![crate::custom_harness::CustomHarness {
        id: "custom-test".into(), name: "Test".into(), icon: "💻".into(),
        executable: "/bin/sh".into(), arguments: Vec::new(),
    }];
    let model = crate::workspace_model::LocalWorkspace::new(state);
    let window = SuperDesktopWindow::new(&app, || {}, Rc::new(crate::hotcorner::Zone::default()), model.state());
    let bar = window.cli_harness_bar.borrow().clone().expect("the local launchers");
    let order = bar.order();
    assert_eq!(&order[..3], ["shell", "codex", "claude"], "the saved order comes back");
    assert_eq!(order.len(), crate::tmux::HARNESS_KEYS.len() + 1, "every launcher keeps a place");
    assert_eq!(order.last().map(String::as_str), Some("custom-test"), "one added since comes last");

    // A drag on a launcher of the local bar saves the order it leaves.
    window.window.set_default_size(1000, 700);
    window.window.present();
    crate::toolbar_reorder::wait_for_layout(&bar.group);
    let (_, custom) = bar.buttons().into_iter().find(|(key, _)| key == "custom-test").unwrap();
    assert!(custom.is_visible(), "a valid custom launcher is offered");
    assert!(crate::toolbar_reorder::drag_to(&bar.group, &custom, -40.0).marked);
    assert_eq!(bar.order().first().map(String::as_str), Some("custom-test"));
    assert_eq!(window.state.borrow().launcher_order, bar.order());
    window.window.close();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn slides_finish_their_visible_motion_quickly() {
    // Hyprland adds nothing on top (the install rule turns layer fades
    // off), so these are the latencies the user sees on every toggle.
    // Target: a toggle is done within 100ms, either way.
    let (x, _) = slide_pose_at(0.0, SLIDE_LAUNCH_IN, 1.0, SLIDE_OMEGA_IN, Duration::from_millis(100));
    assert!(x > 0.99, "slide-in only {:.1}% there after 100ms", x * 100.0);
    let (x, _) = slide_pose_at(1.0, -SLIDE_LAUNCH_OUT, 0.0, SLIDE_OMEGA_OUT, Duration::from_millis(100));
    assert!(x < 0.01, "slide-out still {:.1}% on screen after 100ms", x * 100.0);
    // Critically damped from the launch impulse: never past the target.
    let mut pose = (0.0, SLIDE_LAUNCH_IN);
    for _ in 0..240 {
        pose = spring_step(pose.0, pose.1, 1.0, 1.0 / 240.0, SLIDE_OMEGA_IN);
        assert!(pose.0 <= 1.0 + 1e-9, "slide-in overshoots to {}", pose.0);
    }
}

#[test]
fn slide_moves_a_card_inside_its_own_canvas() {
    crate::gtk_test::run_in_child_process_needing_large_screen("window::tests::slide_inside_own_canvas_inner");
}

#[test]
fn workspace_bounds_follow_output_allocation() {
    crate::gtk_test::run_in_child_process("window::tests::workspace_bounds_follow_output_allocation_inner");
}

#[test]
fn workspace_bounds_follow_output_allocation_inner() {
    if !crate::gtk_test::is_child() {
        return;
    }
    gtk4::init().unwrap();
    let window = gtk4::Window::new();
    window.set_resizable(false);
    let canvas = Fixed::new();
    let hud = gtk4::Box::new(Orientation::Horizontal, 0);
    canvas.put(&hud, 0.0, 0.0);
    let offscreen = Label::new(Some("saved card"));
    canvas.put(&offscreen, 4000.0, 2000.0);
    window.set_child(Some(&desktop_overlay(&canvas)));
    let observed = Rc::new(Cell::new((0, 0)));
    let content = top_bar_content(
        &gtk4::Box::new(Orientation::Horizontal, 0),
        &gtk4::Box::new(Orientation::Horizontal, 0),
        &gtk4::Box::new(Orientation::Horizontal, 0),
    );
    hud.append(&content.widget);
    let fit = TopBarFit::new(&hud, content, Vec::new());
    bind_top_bar_width(&hud, &window, fit, Rc::new({
        let observed = Rc::clone(&observed);
        move |w, h| observed.set((w, h))
    }));
    let fallback = (320, 240);
    assert_eq!(allocated_workspace_size(&window, fallback), fallback);
    for size in [(480, 360), (960, 700), (600, 400), (960, 700), (480, 360)] {
        window.set_default_size(size.0, size.1);
        window.present();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while observed.get() != size && std::time::Instant::now() < deadline {
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(observed.get(), size, "production allocation callback must follow both dimensions");
        assert_eq!(allocated_workspace_size(&window, fallback), size);
        assert_eq!(hud.width_request(), size.0);
        let limits = crate::mini_terminal::workspace_limits(observed.get(), 1.0);
        assert_eq!(limits.right, f64::from(size.0 - 10));
        assert_eq!(limits.bottom, f64::from(size.1 - 10));
    }
    window.close();
}

/// A top bar like the local one, with a real harness bar: every built-in
/// launcher plus `custom` custom ones, a long folder, the right-side
/// actions and the shortcut hint. Returns what the toolbar tests need.
struct TestTopBar {
    hud: gtk4::Box,
    right: gtk4::Box,
    actions: Vec<Button>,
    brand: Label,
    hint: Label,
    launchers: Rc<crate::harness_bar::HarnessBar>,
    fit: Rc<TopBarFit>,
    all_keys: Vec<String>,
    custom: Vec<crate::harness_bar::CustomButton>,
}

fn test_top_bar(custom_count: usize) -> TestTopBar {
    let left = gtk4::Box::new(Orientation::Horizontal, 10);
    let brand = Label::new(Some(BRAND));
    brand.add_css_class("hud-title");
    left.append(&brand);
    let folder = gtk4::Entry::new();
    folder.set_text(&format!("/home/user/{}", "long-project-folder/".repeat(30)));
    folder.set_width_chars(60);
    left.append(&folder);
    let launchers = crate::harness_bar::HarnessBar::new(Rc::new(|_| {}), Rc::new(|_, _| false));
    let custom: Vec<crate::harness_bar::CustomButton> = (0..custom_count)
        .map(|index| crate::harness_bar::CustomButton {
            id: format!("custom-{index}"),
            name: format!("Custom harness {index}"),
            icon: "🧩".into(),
        })
        .collect();
    let all_keys: Vec<String> = crate::tmux::HARNESS_KEYS
        .iter()
        .map(|key| key.to_string())
        .chain(custom.iter().map(|item| item.id.clone()))
        .collect();
    launchers.apply(&crate::harness_bar::HarnessState {
        keys: all_keys.clone(),
        custom: custom.clone(),
        ready: true,
    });
    let right = gtk4::Box::new(Orientation::Horizontal, 10);
    right.set_valign(Align::Center);
    let actions: Vec<_> = [
        "sd-arrange-symbolic",
        "sd-gears-symbolic",
        "sd-hide-symbolic",
    ]
    .into_iter()
    .map(|icon| {
        let button = Button::from_icon_name(icon);
        button.add_css_class("hud-button");
        button.add_css_class("hud-icon-btn");
        right.append(&button);
        button
    })
    .collect();
    let hint = Label::new(Some("[SUPER + SHIFT + Q]"));
    hint.add_css_class("hud-shortcut");
    right.append(&hint);
    let hud = gtk4::Box::new(Orientation::Horizontal, 0);
    hud.add_css_class("hud-bar");
    let content = top_bar_content(&left, &launchers.group, &right);
    hud.append(&content.widget);
    let fit = TopBarFit::new(&hud, content, vec![
        Trim::Hint(hint.clone()),
        Trim::Icons {
            brand: brand.clone(),
            launchers: Rc::new({
                let bar = Rc::clone(&launchers);
                move |compact| bar.set_compact(compact)
            }),
        },
    ]);
    TestTopBar { hud, right, actions, brand, hint, launchers, fit, all_keys, custom }
}

impl TestTopBar {
    /// Offer the first `count` launchers.
    fn offer(&self, count: usize) {
        self.launchers.apply(&crate::harness_bar::HarnessState {
            keys: self.all_keys.iter().take(count).cloned().collect(),
            custom: self.custom.clone(),
            ready: true,
        });
    }

    /// The width the contents need at each level, measured by showing
    /// each in turn; the bar is left at the level it was.
    fn needs(&self) -> Vec<i32> {
        let level = self.fit.level();
        let needs = (0..=2)
            .map(|forced| {
                self.fit.force_level(forced);
                self.fit.needed_width()
            })
            .collect();
        self.fit.force_level(level);
        needs
    }

    /// The level a bar `available` pixels wide should settle at: the
    /// first whose contents fit, or icons alone.
    fn expected_level(needs: &[i32], available: i32) -> usize {
        needs.iter().position(|need| *need <= available).unwrap_or(needs.len() - 1)
    }

    /// What production does over the frames after a width change: fit,
    /// then lay out with the result, until nothing changes.
    fn settle(&self, width: i32) {
        for _ in 0..8 {
            fit_top_bar(&self.hud, width, &self.fit);
            let (_, height, _, _) = self.hud.measure(Orientation::Vertical, width);
            self.hud.allocate(width, height, -1, None);
        }
    }

    fn assert_trimmed_to_fit(&self, width: i32, needs: &[i32]) {
        let available = self.hud.width();
        assert!(
            available > width - 40 && available <= width,
            "the bar's content width {available} must be the display width {width} less its padding"
        );
        let expected = Self::expected_level(needs, available);
        assert_eq!(
            self.fit.level(), expected,
            "at {width} (needs {needs:?}) the bar must give up exactly what does not fit"
        );
        assert_eq!(self.hint.is_visible(), expected == 0, "the hint goes first at {width}");
        assert_eq!(
            self.brand.text(),
            if expected == 2 { BRAND_ICON } else { BRAND },
            "the brand keeps only its icon when the names do not fit at {width}"
        );
        assert_eq!(self.launchers.compact(), expected == 2, "launchers are icons only at level 2 ({width})");
        if expected < 2 {
            assert!(self.fit.needed_width() <= available, "nothing scrolls while names are shown at {width}");
        }
        for key in &self.all_keys {
            if !self.launchers.offered(key) {
                continue;
            }
            let name = self.custom.iter()
                .find(|item| &item.id == key)
                .map(|item| item.name.as_str())
                .unwrap_or_else(|| crate::harness_bar::harness_label(key).0);
            let caption = self.launchers.caption_text(key);
            if expected == 2 {
                assert!(
                    caption.as_deref().is_none_or(|text| !text.contains(name)),
                    "{key} must not show its name ({caption:?}) when the bar is icons only at {width}"
                );
            } else {
                assert!(
                    caption.as_deref().is_some_and(|text| text.contains(name)),
                    "{key} must show its name ({caption:?}) when the bar fits at {width}"
                );
            }
        }
    }

    fn assert_actions_on_screen(&self) {
        let hud = &self.hud;
        let bounds = self.right.compute_bounds(hud).unwrap();
        assert!(bounds.x() >= 0.0);
        // GTK widget coordinates exclude the dock's CSS padding.
        assert!(
            (bounds.x() + bounds.width() - hud.width() as f32).abs() < 1.0,
            "right controls must meet the display edge at {}: {bounds:?}",
            hud.width()
        );
        for button in &self.actions {
            let bounds = button.compute_bounds(hud).unwrap();
            assert!(button.is_visible() && button.is_sensitive());
            assert!(bounds.width() > 0.0 && bounds.height() > 0.0);
            assert!(bounds.x() >= 0.0 && bounds.x() + bounds.width() <= hud.width() as f32);
            assert!(bounds.y() >= 0.0 && bounds.y() + bounds.height() <= hud.height() as f32);
        }
    }
}

#[test]
fn toolbar_trims_names_to_fit_the_display() {
    crate::gtk_test::run_in_child_process("window::tests::toolbar_trims_inner");
}

#[test]
fn toolbar_trims_inner() {
    if !crate::gtk_test::is_child() {
        return;
    }
    gtk4::init().unwrap();
    crate::styles::apply_styles();
    let bar = test_top_bar(24);
    bar.hud.set_width_request(1024);
    fit_top_bar(&bar.hud, 0, &bar.fit);
    assert_eq!(
        bar.hud.width_request(),
        1024,
        "an unmapped window must not collapse the toolbar"
    );
    assert_eq!(bar.fit.level(), 0, "a bar that was never allocated gives nothing up");
    // Somewhere, the icons must fit where the names did not: that is the
    // point of the trim, not only a smaller amount of scrolling.
    let mut icons_fit_where_names_did_not = false;
    let mut hint_went_first = false;
    for count in [0, 3, 12, 36] {
        bar.offer(count);
        for size in [TopBarSize::Small, TopBarSize::Medium, TopBarSize::Large] {
            paint_top_bar_size(&bar.hud, size);
            // Shrink, enlarge, then shrink again, as with output/scale changes.
            // Logical widths also cover scaled outputs, e.g. 1920/1.5=1280
            // and 2560/2=1280.
            for width in [
                1280, 320, 800, 2560, 640, 999, 1000, 1199, 1200, 480, 3440, 1024, 1366, 1600, 320, 3440,
            ] {
                bar.settle(width);
                let needs = bar.needs();
                assert!(needs[0] > needs[1] && needs[1] > needs[2], "each trim must save width: {needs:?}");
                bar.assert_trimmed_to_fit(width, &needs);
                let (_, natural, _, _) = bar.hud.measure(Orientation::Horizontal, -1);
                assert_eq!(natural, width, "contents must not inflate the dock");
                bar.assert_actions_on_screen();
                let available = bar.hud.width();
                if bar.fit.level() == 2 && bar.fit.needed_width() <= available {
                    icons_fit_where_names_did_not = true;
                }
                if bar.fit.level() == 1 {
                    hint_went_first = true;
                }
            }
        }
    }
    assert!(icons_fit_where_names_did_not, "some display must fit the icons but not the names");
    assert!(hint_went_first, "some display must fit everything but the shortcut hint");

    // A launcher turned on while the bar is icons only is icons only too,
    // and the bar follows the contents, not only the width: with fewer
    // launchers the names come back at the same width.
    bar.offer(bar.all_keys.len());
    bar.settle(1280);
    assert_eq!(bar.fit.level(), 2);
    assert_eq!(bar.launchers.caption_text("custom-23").as_deref(), Some("🧩"));
    bar.offer(1);
    bar.settle(1280);
    assert_eq!(bar.fit.level(), 0, "one launcher fits 1280 with its name and the hint");
    assert_eq!(bar.brand.text(), BRAND);
    assert!(bar.hint.is_visible());
    // And back: more launchers at the same width give the names up again.
    bar.offer(bar.all_keys.len());
    bar.settle(1280);
    assert_eq!(bar.fit.level(), 2);
}

#[test]
fn toolbar_controls_stay_on_screen() {
    crate::gtk_test::run_in_child_process_needing_large_screen("window::tests::toolbar_controls_inner");
}

#[test]
fn toolbar_controls_inner() {
    if !crate::gtk_test::is_child() {
        return;
    }
    gtk4::init().unwrap();
    crate::styles::apply_styles();
    let bar = test_top_bar(24);
    bar.offer(bar.all_keys.len());
    let hud = bar.hud.clone();
    // Reproduce the real failure: a saved/off-screen card enlarges Fixed's
    // minimum size, then the toolbar follows that oversized window.
    let canvas = Fixed::new();
    let offscreen = Label::new(Some("off-screen card"));
    offscreen.set_size_request(640, 480);
    canvas.put(&offscreen, 5000.0, 2000.0);
    canvas.put(&hud, 0.0, 0.0);
    let stack = gtk4::Stack::new();
    stack.add_named(&canvas, Some("local"));
    let root = desktop_overlay(&stack);
    assert!(canvas.measure(Orientation::Horizontal, -1).0 > 5000);
    assert_eq!(root.measure(Orientation::Horizontal, -1).0, 0);
    assert_eq!(root.measure(Orientation::Vertical, -1).0, 0);

    let window = gtk4::Window::new();
    window.set_default_size(1024, 600);
    window.set_resizable(false);
    window.set_child(Some(&root));
    bind_top_bar_width(&hud, &window, Rc::clone(&bar.fit), Rc::new(|_, _| {}));
    window.present();
    let check_mapped = |width: i32| {
        let until = std::time::Instant::now() + Duration::from_secs(3);
        let mut settled = 0;
        while std::time::Instant::now() < until {
            while glib::MainContext::default().iteration(false) {}
            let fits = bar.right.compute_bounds(&window).is_some_and(|bounds| {
                bounds.width() > 0.0
                    && bounds.x() >= 0.0
                    && bounds.x() + bounds.width() <= window.width() as f32
                    && hud.width_request() == window.width()
                    && window.width() == width
            });
            settled = if fits { settled + 1 } else { 0 };
            if settled >= 5 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(window.width() > 0 && window.width() < 5000);
        assert_eq!(
            window.width(),
            width,
            "the mapped window must accept the new size"
        );
        assert_eq!(hud.width_request(), window.width());
        let needs = bar.needs();
        bar.assert_trimmed_to_fit(width, &needs);
        let bounds = bar.right.compute_bounds(&window).unwrap();
        assert!(
            bounds.x() >= 0.0 && bounds.x() + bounds.width() <= window.width() as f32,
            "mapped controls {bounds:?} exceed window width {}",
            window.width()
        );
        // Visible geometry alone is insufficient: every action must also be
        // hittable through the mapped window and its clipped canvas.
        for button in &bar.actions {
            let bounds = button.compute_bounds(&window).unwrap();
            let picked = window
                .pick(
                    bounds.x() as f64 + bounds.width() as f64 / 2.0,
                    bounds.y() as f64 + bounds.height() as f64 / 2.0,
                    gtk4::PickFlags::DEFAULT,
                )
                .expect("toolbar action must be hittable");
            assert!(picked == *button || picked.is_ancestor(button));
        }
    };
    for width in [1024, 640, 1600, 480, 1280] {
        window.set_default_size(width, 600);
        check_mapped(width);
    }
    // Sizing is event-driven: an idle, settled overlay must not keep the
    // frame clock running (the old per-frame tick callback did, at the
    // display's refresh rate, for the overlay's whole lifetime).
    let frames = Rc::new(Cell::new(0u32));
    let clock = window.frame_clock().expect("mapped window has a frame clock");
    let counter = Rc::clone(&frames);
    let handler = clock.connect_after_paint(move |_| counter.set(counter.get() + 1));
    let until = std::time::Instant::now() + Duration::from_millis(500);
    while std::time::Instant::now() < until {
        while glib::MainContext::default().iteration(false) {}
        std::thread::sleep(Duration::from_millis(10));
    }
    clock.disconnect(handler);
    assert!(
        frames.get() < 10,
        "an idle toolbar must not repaint every frame ({} frames in 500 ms)",
        frames.get()
    );
    // Hidden, resized while hidden, shown again: the surface may come
    // back realized with a new frame clock, which the bar must follow.
    window.set_visible(false);
    while glib::MainContext::default().iteration(false) {}
    window.set_default_size(900, 600);
    window.set_visible(true);
    check_mapped(900);
    window.set_default_size(1400, 600);
    check_mapped(1400);
    window.close();
}

#[test]
fn slide_inside_own_canvas_inner() {
    if !crate::gtk_test::is_child() {
        return;
    }
    gtk4::init().unwrap();
    let app = gtk4::Application::new(
        Some("com.superdesktop.SlideTest"),
        gtk4::gio::ApplicationFlags::NON_UNIQUE,
    );
    app.register(None::<&gtk4::gio::Cancellable>).unwrap();
    let window = gtk4::ApplicationWindow::new(&app);
    window.set_default_size(800, 600);
    // A remote PC's cards live in a nested canvas, not in the local one: a
    // hide must move them where they are instead of leaving them frozen.
    let outer = gtk4::Fixed::new();
    let inner = gtk4::Fixed::new();
    let card = gtk4::Label::new(Some("remote console"));
    card.set_size_request(200, 100);
    inner.put(&card, 100.0, 120.0);
    outer.put(&inner, 0.0, 0.0);
    window.set_child(Some(&outer));
    window.present();
    // A move takes effect in the next allocation, exactly as it does inside
    // the real vsync slide tick.
    let pump = || {
        let until = std::time::Instant::now() + Duration::from_millis(120);
        while std::time::Instant::now() < until {
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    pump();
    let trajectory = Trajectory {
        sx: -240.0,
        sy: 120.0,
        tx: 100.0,
        ty: 120.0,
    };
    paint_slide_widget(card.upcast_ref(), trajectory, 0.0);
    pump();
    assert_eq!(inner.child_position(&card), (-240.0, 120.0));
    paint_slide_widget(card.upcast_ref(), trajectory, 0.5);
    pump();
    assert_eq!(inner.child_position(&card), (-70.0, 120.0));
    paint_slide_widget(card.upcast_ref(), trajectory, 1.0);
    pump();
    assert_eq!(inner.child_position(&card), (100.0, 120.0));
    // A widget that is not in a Fixed is skipped, never panicked over.
    paint_slide_widget(window.upcast_ref(), trajectory, 0.0);
    window.close();
}

#[test]
fn test_hud_slides_straight_up_from_its_rest_pose() {
    let (tx, ty, sx, sy) = hud_slide_pose(48.0);
    assert_eq!(sx, tx, "toolbar must not drift sideways");
    assert_eq!(ty, 0.0);
    assert!(sy <= -48.0, "hidden toolbar sits fully above the overlay, sy={sy}");
    assert_eq!(tx, 0.0, "full-width toolbar is anchored to the left edge");
}

#[test]
fn test_spring_reverses_without_a_position_jump() {
    let (x, v) = spring_step(0.0, SLIDE_LAUNCH_IN, 1.0, 1.0 / 240.0, SLIDE_OMEGA_IN);
    assert!(x > 0.0 && x < 1.0, "one 240 Hz frame must advance, got {x}");

    // Reverse: same pose, new target. One frame later we are still near x,
    // not teleported to 1.0 or 0.0: the frame moves no further than the
    // one before it did (from 0 to x).
    let (x2, v2) = spring_step(x, v, 0.0, 1.0 / 240.0, SLIDE_OMEGA_OUT);
    assert!(
        (x2 - x).abs() <= x && x2 > 0.0 && x2 < 1.0,
        "a reverse must not jump, {x} -> {x2}"
    );
    assert!(v2 < v, "target 0 must decelerate / reverse velocity, {v} -> {v2}");

    let (frozen_x, frozen_v) = spring_step(0.4, 1.1, 1.0, 0.0, SLIDE_OMEGA_IN);
    assert_eq!((frozen_x, frozen_v), (0.4, 1.1));

    let (mut x, mut v) = (0.0, SLIDE_LAUNCH_IN);
    for _ in 0..240 {
        (x, v) = spring_step(x, v, 1.0, 1.0 / 240.0, SLIDE_OMEGA_IN);
    }
    assert!(
        spring_settled(x, v, 1.0) || (x - 1.0).abs() < 0.02,
        "must settle on-canvas in ~1s at 240 Hz, x={x} v={v}"
    );
}

#[test]
fn test_focused_terminal_rendered_above_any_other_icon_and_sticky_notes() {
    crate::gtk_test::run_in_child_process("window::tests::focused_terminal_stacking_inner");
}

#[test]
fn focused_terminal_stacking_inner() {
    if !crate::gtk_test::is_child() { return; }
    gtk4::init().unwrap();
    let canvas = Fixed::new();

    // Create terminal container 1 (icon/mini terminal)
    let term1 = gtk4::Box::new(Orientation::Vertical, 0);
    // Create terminal container 2 (another icon/terminal)
    let term2 = gtk4::Box::new(Orientation::Vertical, 0);
    canvas.put(&term1, 10.0, 10.0);
    canvas.put(&term2, 20.0, 20.0);

    // Create sticky notes
    let note1_data = NoteData {
        id: "n1".to_string(),
        text: "Note 1".to_string(),
        x: 50,
        y: 50,
        width: 200,
        height: 150,
        color: "omarchy".to_string(),
        updated_at: 0.0,
        tag: 0,
    };
    let note2_data = NoteData {
        id: "n2".to_string(),
        text: "Note 2".to_string(),
        x: 100,
        y: 100,
        width: 200,
        height: 150,
        color: "omarchy".to_string(),
        updated_at: 0.0,
        tag: 0,
    };

    let note1 = StickyNote::for_test(note1_data);
    let note2 = StickyNote::for_test(note2_data);

    canvas.put(&note1.container, 50.0, 50.0);
    canvas.put(&note2.container, 100.0, 100.0);

    // Initially: notes were added after term1 and term2
    assert_eq!(canvas.last_child().as_ref(), Some(note2.container.upcast_ref::<gtk4::Widget>()));

    // Focus terminal 1! Focused terminal must be rendered above any other icon AND even sticky notes!
    raise_canvas_child(&canvas, &term1);
    // Terminal 1 is now the last child (topmost rendered widget)
    assert_eq!(canvas.last_child().as_ref(), Some(term1.upcast_ref::<gtk4::Widget>()));

    // Now focus terminal 2 (an icon/terminal)!
    raise_canvas_child(&canvas, &term2);
    // Terminal 2 is now on top of terminal 1 AND on top of all sticky notes!
    assert_eq!(canvas.last_child().as_ref(), Some(term2.upcast_ref::<gtk4::Widget>()));

    // Now simulate expanding terminal 1 (remove and put to expand to overlay)
    canvas.remove(&term1);
    canvas.put(&term1, 0.0, 0.0);
    // Expanded terminal 1 is at the top of the canvas, above any other icon and even sticky notes!
    assert_eq!(canvas.last_child().as_ref(), Some(term1.upcast_ref::<gtk4::Widget>()));
}
