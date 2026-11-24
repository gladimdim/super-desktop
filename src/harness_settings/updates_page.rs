use super::*;

/// How long a check started by opening Settings stays good; the Updates page
/// and its button always check again.
pub(in crate::harness_settings) const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Settings → Updates (see `updates`): this build's version, a check against
/// the clone's GitHub branch, and the update itself. Returns what starts a
/// check: `true` always, `false` only when the last one is an hour old. The
/// home entry's `entry_chip` names a newer version when a check finds one.
pub(in crate::harness_settings) fn build_updates_page(root: &Box, entry_chip: &Label) -> Rc<dyn Fn(bool)> {
    use crate::updates;
    let current = updates::running();
    let (head, body) = section_card(root, "", "SUPER DESKTOP");
    head.append(&chip(&format!("This build {current}")));

    let text = |classes: &[&str]| {
        let label = Label::new(None);
        for class in classes {
            label.add_css_class(class);
        }
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_wrap_mode(pango::WrapMode::WordChar);
        label
    };
    let status = text(&["launcher-note", "update-status"]);
    status.set_text("Not checked yet.");
    body.append(&status);
    let changes = text(&["update-changes"]);
    changes.set_visible(false);
    body.append(&changes);
    let blocked = text(&["launcher-note", "launcher-note-error"]);
    blocked.set_visible(false);
    body.append(&blocked);
    let actions = Box::new(Orientation::Horizontal, 8);
    let btn_check = Button::with_label("⟳ Check again");
    btn_check.add_css_class("launcher-btn");
    btn_check.set_tooltip_text(Some("Ask GitHub for the newest version"));
    let btn_update = Button::with_label("⬇ Update");
    btn_update.add_css_class("launcher-btn");
    btn_update.add_css_class("launcher-btn-primary");
    btn_update.set_visible(false);
    actions.append(&btn_check);
    actions.append(&btn_update);
    body.append(&actions);
    let source = text(&["launcher-hint"]);
    source.set_selectable(true);
    source.set_visible(false);
    body.append(&source);
    let hint = text(&["launcher-hint"]);
    hint.set_text(
        "Updating moves the clone SUPER DESKTOP runs from to the newest release and rebuilds it with its \
         rebuild.sh. The build runs first, and SUPER DESKTOP restarts on it only if it succeeds; terminal \
         cards keep running. A clone with uncommitted changes or commits of its own is left alone. A copy \
         pinned to a release stays on that version until you switch to the newest here.",
    );
    body.append(&hint);

    let found: Rc<RefCell<Option<updates::Status>>> = Rc::new(RefCell::new(None));
    let checked_at: Rc<Cell<Option<std::time::Instant>>> = Rc::new(Cell::new(None));
    let updating = Rc::new(Cell::new(false));
    let show_error = {
        let status = status.clone();
        move |message: &str| {
            status.add_css_class("launcher-note-error");
            status.set_text(message);
        }
    };

    let apply = {
        let (status, changes, blocked, source) = (status.clone(), changes.clone(), blocked.clone(), source.clone());
        let (btn_check, btn_update, entry_chip) = (btn_check.clone(), btn_update.clone(), entry_chip.clone());
        let (found, updating, show_error) = (Rc::clone(&found), Rc::clone(&updating), show_error.clone());
        move |result: Result<updates::Status, String>| {
            if updating.get() {
                return;
            }
            btn_check.set_sensitive(true);
            status.remove_css_class("launcher-note-error");
            let checked = match result {
                Ok(checked) => checked,
                Err(error) => {
                    show_error(&error);
                    changes.set_visible(false);
                    blocked.set_visible(false);
                    btn_update.set_visible(false);
                    *found.borrow_mut() = None;
                    return;
                }
            };
            let follows = match &checked.release {
                Some(_) => "releases".to_string(),
                None => checked.upstream.clone(),
            };
            source.set_text(&format!("From {} ({follows}) into {}", checked.url, checked.dir.display()));
            source.set_visible(true);
            let pinned = checked.pinned.as_deref();
            if checked.available() {
                status.set_text(&match pinned {
                    Some(tag) => format!(
                        "Pinned to {tag}. SUPER DESKTOP {} is the newest; this build is {}. Switching leaves the pinned version.",
                        checked.latest, checked.current
                    ),
                    None => format!("SUPER DESKTOP {} is available. This build is {}.", checked.latest, checked.current),
                });
                let mut list: Vec<String> = checked.changes.iter().map(|subject| format!("• {subject}")).collect();
                if checked.more > 0 {
                    list.push(format!("…and {} more", checked.more));
                }
                changes.set_text(&list.join("\n"));
                changes.set_visible(!list.is_empty());
                blocked.set_text(checked.blocked.as_deref().unwrap_or(""));
                blocked.set_visible(checked.blocked.is_some());
                btn_update.set_label(&match pinned {
                    Some(_) => format!("⬇ Switch to {}", checked.latest),
                    None => format!("⬇ Update to {}", checked.latest),
                });
                btn_update.set_visible(true);
                btn_update.set_sensitive(checked.blocked.is_none());
                entry_chip.set_text(&format!("{} available", checked.latest));
                entry_chip.add_css_class("update-available");
            } else {
                let newest = match &checked.release {
                    Some(tag) => format!("the newest release is {tag}"),
                    None => format!("the newest on {} is {}", checked.upstream, checked.latest),
                };
                status.set_text(&format!(
                    "{}Up to date. This build is {}; {newest}.",
                    pinned.map(|tag| format!("Pinned to {tag}. ")).unwrap_or_default(),
                    checked.current
                ));
                changes.set_visible(false);
                blocked.set_visible(false);
                btn_update.set_visible(false);
                entry_chip.set_text(&checked.current.to_string());
                entry_chip.remove_css_class("update-available");
            }
            *found.borrow_mut() = Some(checked);
        }
    };
    let refresh = crate::launcher_settings::background_refresh(updates::check, apply);
    let start_check: Rc<dyn Fn(bool)> = {
        let (status, btn_check, btn_update) = (status.clone(), btn_check.clone(), btn_update.clone());
        let (checked_at, updating) = (Rc::clone(&checked_at), Rc::clone(&updating));
        Rc::new(move |force| {
            if updating.get()
                || (!force && checked_at.get().is_some_and(|at| at.elapsed() < UPDATE_CHECK_INTERVAL))
            {
                return;
            }
            checked_at.set(Some(std::time::Instant::now()));
            status.remove_css_class("launcher-note-error");
            status.set_text("Checking GitHub…");
            btn_check.set_sensitive(false);
            btn_update.set_sensitive(false);
            refresh();
        })
    };
    btn_check.connect_clicked({
        let start_check = Rc::clone(&start_check);
        move |_| start_check(true)
    });
    btn_update.connect_clicked({
        let (status, btn_check) = (status.clone(), btn_check.clone());
        move |button| {
            let Some(checked) = found.borrow().clone() else { return };
            updating.set(true);
            button.set_sensitive(false);
            btn_check.set_sensitive(false);
            status.remove_css_class("launcher-note-error");
            status.set_text(&format!(
                "Updating to {}: building. This takes a minute or two, and SUPER DESKTOP restarts on \
                 the new build when it is ready. Terminal cards keep running.",
                checked.latest
            ));
            let (button, btn_check, show_error, updating) =
                (button.clone(), btn_check.clone(), show_error.clone(), Rc::clone(&updating));
            glib::MainContext::default().spawn_local(async move {
                let latest = checked.latest;
                // A successful rebuild replaces this daemon before it ends,
                // so coming back here means it did not.
                let result = gtk4::gio::spawn_blocking(move || {
                    let paths = updates::Paths::user()?;
                    let child = updates::start_update(&checked, &paths)?;
                    updates::wait_rebuild(child, &paths)
                })
                .await
                .unwrap_or_else(|_| Err("the update stopped unexpectedly".to_string()));
                updating.set(false);
                btn_check.set_sensitive(true);
                if let Err(reason) = result {
                    let message = format!("The update to {latest} did not finish: {reason}");
                    show_error(&message);
                    button.set_sensitive(true);
                    gtk4::gio::spawn_blocking(move || updates::notify("SUPER DESKTOP update did not finish", &message));
                }
            });
        }
    });
    start_check
}

