use super::*;

fn term_data(iconified: bool) -> TerminalData {
    TerminalData {
        id: "t".to_string(),
        session_name: "s".to_string(),
        agent_type: "shell".to_string(),
        command: "/usr/bin/bash".to_string(),
        x: 100,
        y: 200,
        width: CARD_WIDTH,
        height: CARD_HEIGHT,
        restored_width: CARD_WIDTH,
        restored_height: CARD_HEIGHT,
        iconified,
        icon_x: None,
        icon_y: None,
        created_at: 0.0,
        tag: 0,
        agent_session_id: None,
        workspace_dir: None,
    }
}

#[test]
fn dropped_card_keeps_the_clamped_origin() {
    crate::gtk_test::run_in_child_process("mini_terminal::tests::dropped_card_clamped_origin_inner");
}

#[test]
fn dropped_card_clamped_origin_inner() {
    if !crate::gtk_test::is_child() { return; }
    gtk4::init().unwrap();
    let canvas = gtk4::Fixed::new();
    let card = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    // Dragged to y 20, then the drop handler clamped it below the top bar.
    canvas.put(&card, 300.0, 20.0);
    canvas.move_(&card, 300.0, 70.0);
    assert_eq!(placed_origin(card.upcast_ref()), Some((300, 70)));
    assert_eq!(placed_origin(gtk4::Box::new(gtk4::Orientation::Vertical, 0).upcast_ref()), None);
}

#[test]
fn cli_close_detaches_without_legacy_cleanup() {
    crate::gtk_test::run_in_child_process("mini_terminal::tests::cli_close_detach_inner");
}

#[test]
fn cli_close_detach_inner() {
    if !crate::gtk_test::is_child() { return; }
    gtk4::init().unwrap();
    let card = MiniTerminalCard::for_test(term_data(true), |_, _| {});
    card.open_with_bare_terminal(480, 320);
    let task = card.cli_session_task();
    let before = std::time::Instant::now();
    assert_eq!(task.with_idle_until(before + Duration::from_secs(1), || card.detach_for_cli_close()), Some(()));
    assert!(task.is_closed());
    assert!(card.vte.borrow().is_none());
    assert!(!task.prepare(|| panic!("closed card must not restart")));
}

#[test]
fn cli_mode_changes_keep_geometry_and_never_start_sessions() {
    crate::gtk_test::run_in_child_process("mini_terminal::tests::cli_mode_changes_inner");
}

#[test]
fn cli_mode_changes_inner() {
    if !crate::gtk_test::is_child() { return; }
    use crate::control::ModeAction::*;
    use std::os::unix::fs::PermissionsExt;
    let tmux = crate::tmux::tmux_bin();
    let server = crate::test_isolation::TmuxServer::new();
    let root = server.directory().to_path_buf();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let socket = server.socket();
    for name in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"] {
        std::env::set_var(name, &root);
    }
    std::env::remove_var("TMUX");
    std::env::remove_var("TMUX_PANE");
    let run = |args: &[&str]| server.command().args(args).output().unwrap();
    let session = "sd_term_mode_missing_suffix";
    assert!(run(&["new-session", "-d", "-s", session, "sleep 60"]).status.success());
    std::fs::write(root.join("bin/tmux"), r#"#!/bin/sh
printf '%s\n' "$*" >> "$SD_MODE_LOG"
exec "$SD_MODE_TMUX" -S "$SD_MODE_SOCKET" -f /dev/null "$@"
"#).unwrap();
    std::fs::set_permissions(root.join("bin/tmux"), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("SD_MODE_TMUX", &tmux);
    std::env::set_var("SD_MODE_SOCKET", &socket);
    std::env::set_var("SD_MODE_LOG", root.join("calls"));
    std::env::set_var("PATH", format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap()));
    gtk4::init().unwrap();
    let mut data = term_data(true);
    data.id = "sd_term_mode_missing".into();
    data.session_name = data.id.clone();
    data.width = icon_side(1.0); data.height = icon_side(1.0);
    data.restored_width = 480; data.restored_height = 320;
    data.icon_x = Some(300); data.icon_y = Some(400);
    let saved = Rc::new(RefCell::new(None));
    let capture = Rc::clone(&saved);
    let card = MiniTerminalCard::for_test(data, move |_, data| { *capture.borrow_mut() = Some(data.clone()); });
    let wait = |done: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(8);
        while !done() {
            assert!(Instant::now() < deadline, "attachment did not settle");
            crate::gtk_test::pump(10);
        }
    };
    for action in [Restore, Expand] {
        assert!(card.cli_set_mode(action, 1024, 768));
        let exited = Rc::new(Cell::new(false));
        let signal = Rc::clone(&exited);
        card.vte.borrow().as_ref().unwrap().connect_child_exited(move |_, status| {
            assert_ne!(status, 0, "missing exact session must fail, not match its prefix");
            signal.set(true);
        });
        wait(&|| exited.get());
        assert!(!card.vte.borrow().as_ref().unwrap().has_focus());
        if action == Restore {
            {
                let saved = saved.borrow(); let d = saved.as_ref().unwrap();
                assert!(!d.iconified);
                assert_eq!((d.width, d.height, d.x, d.y), (480, 320, 100, 200));
                assert_eq!((d.icon_x, d.icon_y), (Some(300), Some(400)));
            }
            assert!(card.cli_set_mode(Minimize, 1024, 768));
        } else {
            assert!(card.is_expanded());
            assert!(card.data.borrow().iconified, "expansion preserves saved mode");
            assert!(card.cli_set_mode(Collapse, 1024, 768));
            assert!(card.is_compact());
            assert!(card.vte.borrow().is_none());
        }
    }
    assert_eq!(String::from_utf8(run(&["list-sessions", "-F", "#{session_name}"]).stdout).unwrap().trim(), session);
    // Restoring an existing session attaches without replacing its process.
    card.data.borrow_mut().session_name = session.into();
    let pane_pid = || run(&["display-message", "-p", "-t", session, "#{pane_pid}"]).stdout;
    let before = pane_pid();
    assert!(card.cli_set_mode(Restore, 1024, 768));
    wait(&|| !run(&["list-clients", "-t", session]).stdout.is_empty());
    assert_eq!(pane_pid(), before);
    assert!(card.cli_set_mode(Minimize, 1024, 768));
    assert_eq!((saved.borrow().as_ref().unwrap().restored_width, saved.borrow().as_ref().unwrap().restored_height), (480, 320));
    let calls = std::fs::read_to_string(root.join("calls")).unwrap();
    assert!(calls.contains("attach-session -t =sd_term_mode_missing"), "{calls}");
    for forbidden in ["new-session", "respawn-pane", "respawn-window", "kill-session"] {
        assert!(!calls.contains(forbidden), "unexpected {forbidden}: {calls}");
    }
}

#[test]
fn cli_geometry_commit_uses_card_callback() {
    crate::gtk_test::run_in_child_process("mini_terminal::tests::cli_geometry_commit_inner");
}

#[test]
fn cli_geometry_commit_inner() {
    if !crate::gtk_test::is_child() { return; }
    gtk4::init().unwrap();
    let saved = Rc::new(RefCell::new(None));
    let capture = Rc::clone(&saved);
    let card = MiniTerminalCard::for_test(term_data(true), move |_, data| { *capture.borrow_mut() = Some(data.clone()); });
    card.open_with_bare_terminal(480, 320);
    assert!(card.apply_geometry(crate::card_resize::Rect { x: 40.0, y: 90.0, width: 640, height: 480 }));
    let data = saved.borrow();
    let data = data.as_ref().expect("geometry must reach persistence callback");
    assert_eq!((data.x, data.y, data.width, data.height), (40, 90, 640, 480));
    assert_eq!((data.restored_width, data.restored_height), (640, 480));
    assert!(!data.iconified);
    assert_eq!((card.container.width_request(), card.container.height_request()), (640, 480));
    assert!(!card.vte.borrow().as_ref().unwrap().has_focus());
}

#[test]
fn test_icon_keeps_its_own_position() {
    // A card that was never minimized draws at the card position.
    let mut d = term_data(false);
    assert_eq!(displayed_pos(&d), (100.0, 200.0));

    // First minimize seeds the icon spot where the card was (no jump).
    d.iconified = true;
    seed_icon_pos(&mut d);
    assert_eq!(displayed_pos(&d), (100.0, 200.0));

    // Dragging the icon moves only the icon spot.
    set_displayed_pos(&mut d, 500, 300);
    assert_eq!((d.x, d.y), (100, 200));
    assert_eq!(displayed_pos(&d), (500.0, 300.0));

    // Restoring returns the card to its own spot.
    d.iconified = false;
    assert_eq!(displayed_pos(&d), (100.0, 200.0));

    // Moving the card leaves the remembered icon spot alone, so the next
    // minimize goes back to it.
    set_displayed_pos(&mut d, 900, 400);
    assert_eq!((d.x, d.y), (900, 400));
    assert_eq!((d.icon_x, d.icon_y), (Some(500), Some(300)));
    d.iconified = true;
    assert_eq!(displayed_pos(&d), (500.0, 300.0));
}

#[test]
fn closing_before_gtk_dispatch_cancels_terminal_preparation() {
    if !crate::gtk_test::is_child() {
        crate::gtk_test::run_in_child_process("mini_terminal::tests::closing_before_gtk_dispatch_cancels_terminal_preparation");
        return;
    }
    if gtk4::init().is_err() { return; }
    let mut data = term_data(false);
    data.session_name = format!("test_sd_cancel_prepare_{}", std::process::id());
    let session = data.session_name.clone();
    let slot = Rc::new(RefCell::new(None));
    let preview = gtk4::Box::new(Orientation::Vertical, 0);
    let task = Arc::new(crate::session_task::SessionTask::default());
    let toggle: Rc<dyn Fn(&TerminalData)> = Rc::new(|_| {});
    spawn_vte(&slot, &preview, &Rc::new(RefCell::new(data)), false,
        &toggle, &Rc::new(RefCell::new(false)), &task, None, HoverRaiseLock::new(),
        &Rc::new(CardActivity::new(Rc::new(|| {}))), None,
        &Rc::new(Cell::new((0.0, 0.0, 1.0))), &RefCell::new(None), true);
    assert!(slot.borrow().is_some(), "placeholder exists before async setup");
    task.close();
    remove_vte(&slot, &preview);
    let context = glib::MainContext::default();
    while context.pending() { context.iteration(false); }
    assert!(slot.borrow().is_none());
    assert!(!crate::tmux::session_exists(&session), "cancelled setup must not create a session");
}

#[test]
fn test_icon_position_round_trips_and_defaults_to_card_position() {
    // State files written before icon_x/icon_y existed must still load.
    let legacy = r#"{"id":"t","session_name":"s","agent_type":"shell",
        "command":"bash","x":10,"y":20,"created_at":0.0}"#;
    let mut old: TerminalData = serde_json::from_str(legacy).unwrap();
    assert_eq!((old.icon_x, old.icon_y), (None, None));
    old.iconified = true;
    assert_eq!(displayed_pos(&old), (10.0, 20.0));

    // New fields survive a save/load cycle.
    let mut d = term_data(true);
    set_displayed_pos(&mut d, 42, 43);
    let json = serde_json::to_string(&d).unwrap();
    let back: TerminalData = serde_json::from_str(&json).unwrap();
    assert_eq!((back.icon_x, back.icon_y), (Some(42), Some(43)));
    assert_eq!((back.x, back.y), (100, 200));
}

#[test]
fn test_expanded_rect_centered() {
    let (x, y, w, h) = expanded_rect(1920, 1080);
    assert_eq!(w, 1536.0);
    assert_eq!(h, 864.0);
    assert_eq!(x, 192.0);
    assert_eq!(y, 108.0);
}

#[test]
fn test_hover_raise_lock_blocks_other_cards_until_expiry_or_click() {
    assert!(NEW_HARNESS_HOVER_LOCK >= Duration::from_secs(3));
    assert!(NEW_HARNESS_HOVER_LOCK <= Duration::from_secs(5));

    let lock = HoverRaiseLock::new();
    assert!(lock.allows_hover("old"));
    assert!(lock.allows_hover("new"));

    lock.lock_for("new", Duration::from_secs(30));
    assert!(lock.allows_hover("new"), "the new harness may still take hover");
    assert!(!lock.allows_hover("old"), "other cards must not steal hover");

    lock.on_click("new");
    assert!(!lock.allows_hover("old"), "clicking the new card keeps the hold");

    lock.on_click("old");
    assert!(lock.allows_hover("old"), "clicking another card is an intentional switch");
    assert!(lock.allows_hover("new"));
}

#[test]
fn hover_dead_zone_uses_live_widget_bounds_and_click_override() {
    if !crate::gtk_test::is_child() {
        crate::gtk_test::run_in_child_process("mini_terminal::tests::hover_dead_zone_uses_live_widget_bounds_and_click_override");
        return;
    }
    if gtk4::init().is_err() { return; }
    let window = gtk4::Window::new();
    let fixed = gtk4::Fixed::new();
    window.set_child(Some(&fixed));
    let card = gtk4::Box::new(Orientation::Vertical, 0);
    card.set_size_request(100, 80);
    fixed.put(&card, 20.0, 20.0);
    window.set_default_size(300, 200);
    window.present();
    for _ in 0..30 {
        while glib::MainContext::default().iteration(false) {}
        if card.is_mapped() && card.width() > 0 { break; }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(card.is_mapped());
    let lock = HoverRaiseLock::new();
    lock.note_hover("front", &card);
    let point = card.compute_point(&fixed, &gtk4::graphene::Point::new(card.width() as f32, 40.0)).unwrap();
    let x = f64::from(point.x());
    let y = f64::from(point.y());
    assert!(!lock.allows_hover_at("back", &fixed, x + 20.0, y));
    assert!(lock.allows_hover_at("back", &fixed, x + 21.0, y));
    assert!(lock.allows_hover_at("front", &fixed, x + 2.0, y));
    lock.on_click("front");
    assert!(!lock.allows_hover_at("back", &fixed, x + 2.0, y));
    lock.on_click("back");
    assert!(lock.allows_hover_at("back", &fixed, x + 2.0, y));
    lock.note_hover("front", &card);
    card.set_visible(false);
    assert!(lock.allows_hover_at("back", &fixed, x + 2.0, y));
    fixed.remove(&card);
    drop(card);
    assert!(lock.allows_hover_at("back", &fixed, x + 2.0, y));
    window.close();
}

#[test]
fn test_hover_dead_zone_edges_and_corners() {
    for (x, y) in [(-20.0, 40.0), (120.0, 40.0), (50.0, -20.0),
        (50.0, 100.0), (-20.0, -20.0), (120.0, 100.0)] {
        assert!(inside_hover_margin(x, y, 100, 80));
    }
    for (x, y) in [(-20.01, 40.0), (120.01, 40.0), (50.0, -20.01), (50.0, 100.01)] {
        assert!(!inside_hover_margin(x, y, 100, 80));
    }
    assert!(inside_hover_margin(117.0, 40.0, 100, 80));
    assert!(!inside_hover_margin(117.0, 40.0, 90, 80));
}

#[test]
fn resize_release_holds_hover_for_one_second_then_allows_other_cards() {
    assert_eq!(RESIZE_HOVER_LOCK, Duration::from_secs(1));
    let lock = HoverRaiseLock::new();
    let before_release = Instant::now();
    lock.lock_for("resized", RESIZE_HOVER_LOCK);
    assert!(lock.until.get().unwrap() >= before_release + Duration::from_secs(1));
    assert!(lock.allows_hover("resized"));
    assert!(!lock.allows_hover("under_pointer"));
    // Advance the deadline without a slow, timing-sensitive sleep.
    lock.until.set(Some(Instant::now()));
    assert!(lock.allows_hover("under_pointer"));
}

#[test]
fn test_hover_raise_lock_expires() {
    let lock = HoverRaiseLock::new();
    lock.lock_for("new", Duration::ZERO);
    assert!(lock.allows_hover("old"));
    assert!(lock.allows_hover("new"));
}

#[test]
fn test_new_terminal_default_size_is_640x480() {
    assert_eq!((NEW_TERM_WIDTH, NEW_TERM_HEIGHT), (640, 480));
    // Unclamped on a typical screen.
    let (w, h) = clamp_card_size(NEW_TERM_WIDTH, NEW_TERM_HEIGHT, 1920, 1080);
    assert_eq!((w, h), (640, 480));
}

#[test]
fn test_clamp_card_size_bounds() {
    let (w, h) = clamp_card_size(10, 10, 1920, 1080);
    assert_eq!((w, h), (MIN_CARD_WIDTH, MIN_CARD_HEIGHT));
    // The whole screen inside the 10 px margins and under the toolbar.
    let (w, h) = clamp_card_size(99999, 99999, 1920, 1080);
    assert_eq!(w, 1900);
    assert_eq!(h, 1000);
}

#[test]
fn test_card_edges_resize_up_to_the_screen_margins() {
    use crate::card_resize::{resized_rect, Edge, Rect};
    // A card part-way down a 3440×1440 screen: its bottom edge goes all
    // the way to the 10 px margin, not to a fraction of the screen height.
    let limits = workspace_limits((3440, 1440), 1.0);
    let start = Rect { x: 1822.0, y: 192.0, width: 994, height: 1051 };
    let down = resized_rect(Edge::South, start, 0.0, 5000.0, limits);
    assert_eq!(down.y + down.height as f64, 1430.0);
    let right = resized_rect(Edge::East, start, 5000.0, 0.0, limits);
    assert_eq!(right.x + right.width as f64, 3430.0);
    // Dragged from the far corner, the card fills the usable screen.
    let start = Rect { x: 10.0, y: 70.0, width: 640, height: 480 };
    let full = resized_rect(Edge::SouthEast, start, 9000.0, 9000.0, limits);
    assert_eq!((full.width, full.height), (3420, 1360));
    // A fitted remote view keeps the same margins in its own pixels.
    let fitted = workspace_limits((1720, 720), 0.5);
    assert_eq!((fitted.max_width, fitted.max_height), (1710, 680));
    assert_eq!(fitted.bottom, 715.0);
}

#[test]
fn test_status_view_texts_mapping() {
    assert_eq!(status_view_texts("WORKING"), ("● WORKING", "●", "status-busy"));
    assert_eq!(status_view_texts("BUSY"), ("● WORKING", "●", "status-busy"));
    assert_eq!(status_view_texts("EXITED"), ("○ EXITED", "○", "status-exited"));
    assert_eq!(status_view_texts("IDLE"), ("● IDLE", "●", "status-idle"));
    assert_eq!(status_view_texts("weird"), ("● IDLE", "●", "status-idle"));
}

#[test]
fn test_format_card_title_with_and_without_prompt() {
    assert_eq!(
        format_card_title("⚡ Claude Code", Some("fix login bug")),
        "⚡ Claude Code • fix login bug"
    );
    assert_eq!(format_card_title("⚡ Claude Code", None), "⚡ Claude Code");
    assert_eq!(format_card_title("⚡ Claude Code", Some("")), "⚡ Claude Code");
    assert_eq!(format_card_title("⚡ Claude Code", Some("   ")), "⚡ Claude Code");
}

/// The outline rule hangs on this: a card counts as "in use" while the
/// pointer is on it or it is taking keystrokes — and stops counting on its
/// own. GTK focus cannot be used here (hover-raise grabs it and nothing
/// releases it), which is what made the outlines vanish for good.
#[test]
fn a_card_is_active_only_while_the_user_is_in_it() {
    let changes = Rc::new(Cell::new(0));
    let counter = Rc::clone(&changes);
    let activity = CardActivity::new(Rc::new(move || counter.set(counter.get() + 1)));
    let start = Instant::now();

    assert!(!activity.is_active(start), "an untouched card is not in use");

    activity.set_pointer_inside(true);
    assert!(activity.is_active(start), "the pointer counts at once");
    activity.set_pointer_inside(true);
    assert_eq!(changes.get(), 1, "an unchanged pointer must not redraw");
    activity.set_pointer_inside(false);
    assert_eq!(changes.get(), 2);

    assert!(!activity.is_active(start));
    activity.note_activity(start);
    assert_eq!(changes.get(), 3, "typing makes the card active");
    assert!(activity.is_active(start + Duration::from_secs(1)));
    activity.note_activity(start + Duration::from_secs(2));
    assert_eq!(changes.get(), 3, "keystrokes while active change nothing");

    // A pause hands the card back, and the next key takes it again. The
    // window runs from the *last* keystroke, not from the first.
    let expired = start + Duration::from_secs(2) + ACTIVE_FOR;
    assert!(activity.is_active(expired - Duration::from_millis(1)));
    assert!(!activity.is_active(expired));
    activity.note_activity(expired);
    assert_eq!(changes.get(), 4);

    // The pointer wins over an expired keyboard state.
    activity.set_pointer_inside(true);
    assert!(activity.is_active(start + Duration::from_secs(600)));
}

#[test]
fn a_covered_terminal_stops_drawing_without_resizing_tmux() {
    crate::gtk_test::run_in_child_process("mini_terminal::tests::vte_drawing_gtk");
}

/// Pausing a buried terminal must look like nothing happened to it: the
/// card keeps its size, the PTY (tmux's view of the window) keeps its
/// grid, output keeps being read, and the whole-window hide and the cover
/// compose instead of overriding each other.
#[test]
fn vte_drawing_gtk() {
    if !crate::gtk_test::is_child() {
        return;
    }
    gtk4::init().unwrap();
    let pump = || {
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let window = gtk4::Window::new();
    // The card's own layout: a fixed-size body whose preview box holds the
    // terminal.
    let card = gtk4::Box::new(Orientation::Vertical, 0);
    card.set_size_request(480, 320);
    let header = Label::new(Some("header"));
    card.append(&header);
    let preview_box = gtk4::Box::new(Orientation::Vertical, 0);
    preview_box.set_vexpand(true);
    card.append(&preview_box);
    let term = VteTerminal::new();
    term.set_hexpand(true);
    term.set_vexpand(true);
    preview_box.append(&term);
    // A PTY with no child process: its window size is what an attached
    // tmux client would be told.
    let pty = term
        .pty_new_sync(PtyFlags::DEFAULT, None::<&gtk4::gio::Cancellable>)
        .unwrap();
    term.set_pty(Some(&pty));
    window.set_child(Some(&card));
    window.present();
    pump();

    let card_size = (card.width(), card.height());
    let grid = (term.column_count(), term.row_count());
    assert!(grid.0 > 20 && grid.1 > 5, "laid out at the card's size: {grid:?}");
    let pty_size = pty.size().unwrap();
    assert_eq!(pty_size, (grid.1 as i32, grid.0 as i32));

    let drawing = VteDrawing::new();
    assert!(drawing.drawing());
    drawing.set_covered(true, Some(&term));
    assert!(!term.is_visible() && !drawing.drawing());
    term.feed(b"written while buried\r\n");
    pump();
    assert!(!term.is_mapped());
    assert_eq!((card.width(), card.height()), card_size, "the card keeps its size");
    assert_eq!(pty.size().unwrap(), pty_size, "tmux sees no resize");
    let text = term.text_format(vte4::Format::Text).unwrap_or_default();
    assert!(
        text.contains("written while buried"),
        "a paused terminal keeps reading output: {text:?}"
    );

    // The overlay hides and shows while the terminal is buried: the show
    // must not bring it back.
    drawing.set_mapped(false, Some(&term));
    drawing.set_mapped(true, Some(&term));
    assert!(!term.is_visible(), "a show left the buried terminal paused");

    // Uncovered while the overlay is hidden: it waits for the show.
    drawing.set_mapped(false, Some(&term));
    drawing.set_covered(false, Some(&term));
    assert!(!term.is_visible());
    drawing.set_mapped(true, Some(&term));
    assert!(term.is_visible());
    // Shown again, it is laid out anew on the next frame (Broadway runs
    // few): only then would a changed size reach the PTY.
    let deadline = Instant::now() + Duration::from_secs(10);
    while term.width() <= 0 && Instant::now() < deadline {
        pump();
    }
    assert!(term.is_mapped() && term.width() > 0, "laid out again");
    assert_eq!((term.column_count(), term.row_count()), grid);
    assert_eq!(pty.size().unwrap(), pty_size, "tmux sees no resize");
    assert_eq!((card.width(), card.height()), card_size);
    let text = term.text_format(vte4::Format::Text).unwrap_or_default();
    assert!(
        text.contains("written while buried"),
        "the terminal shows what arrived while it was paused: {text:?}"
    );

    // A terminal re-created while its card is buried (a restore) starts
    // visible; the next decision brings it in line.
    drawing.set_covered(true, Some(&term));
    let fresh = VteTerminal::new();
    preview_box.append(&fresh);
    assert!(fresh.is_visible());
    drawing.set_covered(true, Some(&fresh));
    assert!(!fresh.is_visible());
    window.close();
}

#[test]
fn slide_freeze_keeps_the_terminal_and_its_grid() {
    crate::gtk_test::run_in_child_process("mini_terminal::tests::slide_freeze_gtk");
}

/// A still image stands in for a terminal during a slide: same place and
/// size, no resize reaching tmux, output still read, and only the end of
/// the slide brings the live terminal back.
#[test]
fn slide_freeze_gtk() {
    if !crate::gtk_test::is_child() {
        return;
    }
    gtk4::init().unwrap();
    let pump = || {
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let window = gtk4::Window::new();
    let card = gtk4::Box::new(Orientation::Vertical, 0);
    card.set_size_request(480, 320);
    card.append(&Label::new(Some("header")));
    let preview_box = gtk4::Box::new(Orientation::Vertical, 0);
    preview_box.set_vexpand(true);
    card.append(&preview_box);
    let term = VteTerminal::new();
    term.set_hexpand(true);
    term.set_vexpand(true);
    preview_box.append(&term);
    // Configured as in `MiniTerminalCard::new`.
    let frame = gtk4::Picture::new();
    frame.set_can_shrink(true);
    frame.set_content_fit(gtk4::ContentFit::Fill);
    frame.set_hexpand(true);
    frame.set_vexpand(true);
    frame.set_visible(false);
    preview_box.append(&frame);
    let pty = term
        .pty_new_sync(PtyFlags::DEFAULT, None::<&gtk4::gio::Cancellable>)
        .unwrap();
    term.set_pty(Some(&pty));
    window.set_child(Some(&card));
    let drawing = VteDrawing::new();

    // Not laid out yet: try again later.
    assert!(!freeze_terminal(Some(&term), &drawing, &frame));
    assert!(!drawing.frozen() && term.get_visible() && !frame.get_visible());

    window.present();
    let deadline = Instant::now() + Duration::from_secs(10);
    while term.width() <= 0 && Instant::now() < deadline {
        pump();
    }
    let card_size = (card.width(), card.height());
    let term_size = (term.width(), term.height());
    // The allocation the terminal leaves, CSS padding included.
    let border_box = term
        .compute_bounds(&term)
        .map(|bounds| (bounds.width() as i32, bounds.height() as i32))
        .unwrap();
    let pty_size = pty.size().unwrap();
    term.feed(b"before the slide\r\n");
    pump();
    // Output since the last painted frame (a busy agent): GTK has dropped
    // the terminal's cached node, and the freeze must still succeed.
    term.feed(b"just printed\r\n");

    assert!(freeze_terminal(Some(&term), &drawing, &frame));
    assert!(drawing.frozen() && !drawing.drawing());
    assert!(!term.is_visible() && frame.is_visible());
    let texture = frame.paintable().expect("the slide shows a texture");
    let scale = term.scale_factor();
    assert_eq!(
        (texture.intrinsic_width(), texture.intrinsic_height()),
        (border_box.0 * scale, border_box.1 * scale),
        "rendered 1:1 at the terminal's size"
    );
    // Frozen again (a reversed slide): the same image stays.
    assert!(freeze_terminal(Some(&term), &drawing, &frame));
    assert_eq!(frame.paintable().as_ref(), Some(&texture));

    // A show reuses the hide's image only while it still shows this
    // terminal: same emulator and look, nothing printed since.
    // VTE reports output after processing it: let the earlier output's
    // report land first (in the desktop it errs on the safe side).
    pump();
    let watch = SlideFrameWatch::default();
    let look = (480, 320, false, false);
    watch.watch(&term, look);
    pump();
    assert!(watch.still_shows(Some(&term), look));
    assert!(!watch.still_shows(Some(&term), (640, 320, false, false)), "resized");
    assert!(!watch.still_shows(Some(&term), (480, 320, true, false)), "expanded");
    assert!(!watch.still_shows(Some(&VteTerminal::new()), look), "another emulator");
    assert!(!watch.still_shows(None, look), "no emulator");
    term.feed(b"printed while hidden\r\n");
    pump();
    assert!(!watch.still_shows(Some(&term), look), "output since the image");
    watch.watch(&term, look);
    assert!(watch.still_shows(Some(&term), look), "a new image");
    watch.forget();
    assert!(!watch.still_shows(Some(&term), look));

    term.feed(b"written while sliding\r\n");
    pump();
    // Broadway can take a few frames to lay the image out.
    let deadline = Instant::now() + Duration::from_secs(10);
    while frame.width() <= 0 && Instant::now() < deadline {
        pump();
    }
    assert_eq!(
        (frame.width(), frame.height()),
        border_box,
        "the image takes the terminal's place exactly"
    );
    assert_eq!((card.width(), card.height()), card_size, "the card keeps its size");
    assert_eq!(pty.size().unwrap(), pty_size, "tmux sees no resize");

    // Showing the overlay or uncovering the card does not end the slide.
    drawing.set_mapped(true, Some(&term));
    drawing.set_covered(false, Some(&term));
    assert!(!term.is_visible() && frame.is_visible());

    thaw_terminal(Some(&term), &drawing, &frame);
    assert!(!drawing.frozen() && drawing.drawing());
    assert!(term.is_visible() && !frame.is_visible());
    assert!(frame.paintable().is_none(), "the texture is released");
    let deadline = Instant::now() + Duration::from_secs(10);
    while term.width() <= 0 && Instant::now() < deadline {
        pump();
    }
    pump();
    assert_eq!((term.width(), term.height()), term_size);
    assert_eq!(pty.size().unwrap(), pty_size, "tmux sees no resize");
    let text = term.text_format(vte4::Format::Text).unwrap_or_default();
    assert!(
        text.contains("before the slide") && text.contains("written while sliding"),
        "the terminal kept reading while it was frozen: {text:?}"
    );

    // Unmapped while frozen (the hide's unmap): thawing must not show the
    // terminal, and an unmapped terminal has nothing to freeze.
    assert!(freeze_terminal(Some(&term), &drawing, &frame));
    drawing.set_mapped(false, Some(&term));
    thaw_terminal(Some(&term), &drawing, &frame);
    assert!(!term.is_visible() && !frame.is_visible());
    assert!(freeze_terminal(Some(&term), &drawing, &frame));
    assert!(!drawing.frozen(), "nothing on screen to freeze");
    window.close();
}

#[test]
fn jump_to_newest_shows_while_the_pane_is_scrolled_back() {
    crate::gtk_test::run_in_child_process("mini_terminal::tests::jump_to_newest_gtk");
}

/// A local card over a private tmux server's session: scrolling back shows
/// the button, clicking it returns the pane to its newest output, and an
/// icon never shows it.
#[test]
fn jump_to_newest_gtk() {
    if !crate::gtk_test::is_child() {
        return;
    }
    gtk4::init().unwrap();
    crate::styles::apply_styles();
    let session = "sd_term_jump_newest";
    let server = crate::scrollback::tests::Server::with_history(session);
    let scrollback = server.scrollback();
    let wait = |what: &str, done: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out: {what}");
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let data = crate::state::TerminalData {
        id: session.to_string(),
        session_name: session.to_string(),
        agent_type: "claude".to_string(),
        command: "claude".to_string(),
        x: 20,
        y: 80,
        width: 480,
        height: 320,
        restored_width: 480,
        restored_height: 320,
        // No emulator until the bare one below: nothing attaches to tmux.
        iconified: true,
        icon_x: None,
        icon_y: None,
        created_at: 0.0,
        tag: 0,
        agent_session_id: None,
        workspace_dir: None,
    };
    let card = Rc::new(MiniTerminalCard::for_test(data, |_, _| {}));
    card.jump.use_scrollback(scrollback.clone());
    card.open_with_bare_terminal(480, 320);
    let window = gtk4::Window::new();
    let canvas = gtk4::Fixed::new();
    canvas.put(&card.container, 20.0, 20.0);
    window.set_child(Some(&canvas));
    window.set_default_size(700, 500);
    window.present();
    let button = card.jump.button.clone();
    assert!(!button.is_visible(), "a card following its output shows no button");
    assert!(!button.is_focusable(), "the keyboard stays with the terminal");

    // The mouse wheel over the terminal: tmux enters copy mode, and the
    // card asks tmux once the scrolling stops. The terminal still gets it.
    server.scroll_back(session);
    let scroll = card.preview_box.observe_controllers().into_iter()
        .filter_map(|controller| controller.ok()?.downcast::<gtk4::EventControllerScroll>().ok())
        .next()
        .expect("the terminal area watches scrolling");
    assert!(!scroll.emit_by_name::<bool>("scroll", &[&0.0f64, &-1.0f64]), "scrolling reaches the terminal");
    wait("the button shows after scrolling back", &|| button.is_visible());
    assert!(button.is_mapped());
    assert!(button.width() < card.container.width(), "a pill, not a bar");

    // Clicking returns the pane to its newest output. A listing taken
    // before the jump does not bring the button back.
    button.emit_clicked();
    assert!(!button.is_visible());
    wait("copy mode ends", &|| scrollback.in_mode(session) == Some(false));
    card.jump.listed(true);
    assert!(!button.is_visible(), "a stale listing is not believed right after a jump");

    // Typing while it shows: `q` ends copy mode in tmux, and the card
    // notices without waiting for the next listing.
    server.scroll_back(session);
    card.jump.check_soon();
    wait("the button shows again", &|| button.is_visible());
    let keys = card.preview_box.observe_controllers().into_iter()
        .filter_map(|controller| controller.ok()?.downcast::<gtk4::EventControllerKey>().ok())
        .next()
        .expect("the terminal area watches keys");
    server.run(&["send-keys", "-t", &format!("={session}:"), "-X", "cancel"]);
    assert!(!keys.emit_by_name::<bool>("key-pressed", &[&gdk::Key::q, &24u32, &gdk::ModifierType::empty()]));
    wait("the button hides after copy mode ends", &|| !button.is_visible());

    // An icon has no terminal to jump in.
    server.scroll_back(session);
    card.jump.check_soon();
    wait("the button shows before iconifying", &|| button.is_visible());
    assert!(card.set_iconified(true));
    assert!(!button.is_visible());
    card.jump.listed(true);
    card.jump.check_soon();
    let settle = Instant::now() + Duration::from_millis(400);
    while Instant::now() < settle {
        while glib::MainContext::default().iteration(false) {}
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!button.is_visible(), "an icon never shows the button");
    window.close();
}
