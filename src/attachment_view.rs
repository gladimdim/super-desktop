//! A card's 📎 attachments: files on this PC the user collects for the
//! harness, then inserts into its prompt in one go.
//!
//! The 📎 button in a local card's header, next to Files, opens a floating panel with the
//! files attached so far and a browser of this PC's folders, starting in the
//! card's project folder. Files can also be dropped on the panel from a file
//! manager or pasted with Ctrl+V. **Insert into prompt** types them into the
//! harness's composer the way phone attachments are delivered: images become
//! the harness's own `[Image …]` attachments where it supports that, every
//! other file is named by its path (`prompt_attachments::insert_local`).
//! Nothing is copied and nothing is submitted: the user finishes the prompt.
//!
//! The list lives in memory per card until it is inserted or cleared, so
//! closing the panel keeps it. There is no system file dialog: the overlay is
//! a layer-shell surface above every window, and a portal dialog would open
//! underneath it.
use gtk4::{gdk, gio, glib, prelude::*, Align, Orientation};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::prompt_attachments::{Kind, MAX_LOCAL_ATTACHMENTS};
use crate::theme::OmarchyTheme;

const DEFAULT_SIZE: (i32, i32) = (620, 620);
const MIN_SIZE: (i32, i32) = (420, 440);
/// Entries shown from one folder; a huge folder is cut here.
const LIST_CAP: usize = 500;

thread_local! {
    /// Files attached to each card (by tmux session), in the order added.
    static STAGED: RefCell<HashMap<String, Vec<PathBuf>>> = RefCell::new(HashMap::new());
    /// Every card's 📎 button, repainted with its count.
    static BUTTONS: RefCell<Vec<(String, glib::WeakRef<gtk4::Button>)>> = const { RefCell::new(Vec::new()) };
    static PANELS: crate::floating_panel::CardPanels = crate::floating_panel::CardPanels::new(DEFAULT_SIZE, MIN_SIZE);
}

/// The files attached to `session`.
pub fn staged(session: &str) -> Vec<PathBuf> {
    STAGED.with(|staged| staged.borrow().get(session).cloned().unwrap_or_default())
}

fn set_staged(session: &str, files: Vec<PathBuf>) {
    STAGED.with(|staged| {
        let mut staged = staged.borrow_mut();
        if files.is_empty() {
            staged.remove(session);
        } else {
            staged.insert(session.to_string(), files);
        }
    });
    let count = staged(session).len();
    BUTTONS.with(|buttons| {
        buttons.borrow_mut().retain(|(owner, weak)| match weak.upgrade() {
            Some(button) => {
                if owner == session {
                    paint_button(&button, count);
                }
                true
            }
            None => false,
        })
    });
}

/// Add `new` to `files`, skipping duplicates and anything past the limit.
/// Returns how many were left out for the limit.
fn add_paths(files: &mut Vec<PathBuf>, new: impl IntoIterator<Item = PathBuf>) -> usize {
    let mut over = 0;
    for path in new {
        if files.contains(&path) {
            continue;
        }
        if files.len() >= MAX_LOCAL_ATTACHMENTS {
            over += 1;
            continue;
        }
        files.push(path);
    }
    over
}

fn paint_button(button: &gtk4::Button, count: usize) {
    let tip = match count {
        0 => "Attach files to this harness".to_string(),
        1 => "1 file attached; open to insert it into the prompt".to_string(),
        n => format!("{n} files attached; open to insert them into the prompt"),
    };
    button.set_tooltip_text(Some(&tip));
    if let Some(label) = count_label(button) {
        label.set_text(&count.to_string());
        label.set_visible(count > 0);
    }
    if count > 0 {
        button.add_css_class("has-attachments");
    } else {
        button.remove_css_class("has-attachments");
    }
}

/// The paper clip: ours, or the icon theme's when an older copy of the
/// installed assets does not have ours yet.
fn attach_icon() -> &'static str {
    let ours = gdk::Display::default()
        .is_none_or(|display| gtk4::IconTheme::for_display(&display).has_icon("sd-attach-symbolic"));
    if ours { "sd-attach-symbolic" } else { "mail-attachment-symbolic" }
}

/// The count badge on a 📎 button.
fn count_label(button: &gtk4::Button) -> Option<gtk4::Label> {
    button.child().and_then(|clip| clip.last_child()).and_downcast::<gtk4::Label>()
}

/// The 📎 header button of a local card. `folder` is where the browser
/// starts: the card's project folder.
pub fn button(session: String, title: String, folder: String) -> gtk4::Button {
    // The paper clip, with how many files wait to be inserted drawn over its
    // corner: the header sets the card's minimum width, so a count beside
    // the clip would widen every card that has files waiting.
    let clip = gtk4::Overlay::new();
    let image = gtk4::Image::from_icon_name(attach_icon());
    image.set_pixel_size(16);
    clip.set_child(Some(&image));
    let count = gtk4::Label::new(None);
    count.add_css_class("attach-count");
    count.set_halign(Align::End);
    count.set_valign(Align::Start);
    count.set_can_target(false);
    clip.add_overlay(&count);
    let button = gtk4::Button::new();
    button.set_child(Some(&clip));
    button.update_property(&[gtk4::accessible::Property::Label("Attach files")]);
    button.add_css_class("term-btn");
    button.add_css_class("attach-btn");
    paint_button(&button, staged(&session).len());
    BUTTONS.with(|buttons| buttons.borrow_mut().push((session.clone(), button.downgrade())));
    let origin = button.downgrade();
    button.connect_clicked(move |_| open(&session, &title, &folder, origin.clone()));
    button
}

/// Show the attachments panel of `session`, in front, or bring it there.
fn open(session: &str, title: &str, folder: &str, origin: glib::WeakRef<gtk4::Button>) {
    if !PANELS.with(|panels| panels.wants_new(session)) {
        return;
    }
    let drawer = build(session, title, Path::new(folder), origin);
    let target = session.to_string();
    drawer.close.connect_clicked(move |_| close(&target));
    // The panel's repaint. Its rows and buttons reach it only weakly (they
    // live inside the panel it redraws), so the panel keeps it for as long
    // as it is open.
    let paint = Box::new(Rc::clone(&drawer.paint));
    PANELS.with(|panels| panels.show(session, &drawer.widget, Rc::clone(&drawer.generation), Some(paint)));
}

/// Close the attachments panel of `session`. Its files stay attached.
fn close(session: &str) {
    PANELS.with(|panels| panels.close(session));
}

/// Give the keyboard to the terminal of the card `button` sits on.
fn focus_card_terminal(button: &gtk4::Button) {
    let mut card = button.parent();
    while let Some(widget) = card.as_ref() {
        if widget.has_css_class("mini-terminal") {
            break;
        }
        card = widget.parent();
    }
    let Some(card) = card else { return };
    let mut stack = vec![card];
    while let Some(widget) = stack.pop() {
        if widget.has_css_class("term-vte") {
            widget.grab_focus();
            return;
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            child = next.next_sibling();
            stack.push(next);
        }
    }
}

/// One folder entry in the browser.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    path: PathBuf,
    name: String,
    is_dir: bool,
    size: u64,
}

/// A folder's entries: folders first, then files, by name; hidden ones only
/// when asked; at most [`LIST_CAP`]. The flag says the list was cut.
fn list_folder(folder: &Path, hidden: bool) -> std::io::Result<(Vec<Entry>, bool)> {
    let mut entries: Vec<Entry> = std::fs::read_dir(folder)?
        .filter_map(Result::ok)
        .filter_map(|item| {
            let name = item.file_name().to_string_lossy().into_owned();
            if !hidden && name.starts_with('.') {
                return None;
            }
            let path = item.path();
            // Follows links, so a linked folder opens like a folder.
            let meta = std::fs::metadata(&path).ok()?;
            Some(Entry { path, name, is_dir: meta.is_dir(), size: meta.len() })
        })
        .collect();
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    let cut = entries.len() > LIST_CAP;
    entries.truncate(LIST_CAP);
    Ok((entries, cut))
}

fn file_icon(path: &Path) -> &'static str {
    if path.is_dir() {
        "📁"
    } else if crate::prompt_attachments::local_kind(path) == Kind::Image {
        "🖼"
    } else {
        "📄"
    }
}

/// "~/code/app · 1.2 MB" for an attached file.
fn describe(path: &Path) -> String {
    let parent = path.parent().map(|p| crate::state::display_dir(&p.to_string_lossy())).unwrap_or_default();
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => format!("{parent} · folder"),
        Ok(meta) => format!("{parent} · {}", glib::format_size(meta.len())),
        Err(_) => format!("{parent} · missing"),
    }
}

/// What the panel says when Insert fails.
fn explain(error: &str) -> String {
    if let Some(name) = error.strip_prefix("missing:") {
        return format!("{name} no longer exists. Remove it and try again.");
    }
    match error {
        "no_such_session" => "This terminal is no longer running.".into(),
        "terminal_input_busy_try_again" => "The terminal is taking other input right now. Try again.".into(),
        "too_many_attachments" => format!("At most {MAX_LOCAL_ATTACHMENTS} files at a time."),
        "image_not_confirmed" => {
            "The harness did not show the image as attached. Check its prompt before trying again.".into()
        }
        "invalid_prompt" => "A file name has characters a terminal cannot take.".into(),
        other => format!("Could not insert the files: {other}"),
    }
}

// `insert`, `status`, `add` and `browse` are for the tests.
#[cfg_attr(not(test), allow(dead_code))]
struct Drawer {
    widget: gtk4::Box,
    close: gtk4::Button,
    generation: Rc<Cell<u64>>,
    insert: gtk4::Button,
    status: gtk4::Label,
    /// Repaints the attached list and the browser's checks. Everything in
    /// the panel holds it weakly: whoever shows the panel must keep this.
    paint: Rc<dyn Fn()>,
    /// Adds files, as a drop or a paste does.
    add: Rc<dyn Fn(Vec<PathBuf>)>,
    /// Opens a folder in the browser.
    browse: Rc<dyn Fn(PathBuf)>,
}

fn small_button(label: &str, tooltip: &str) -> gtk4::Button {
    let button = gtk4::Button::with_label(label);
    button.add_css_class("launcher-btn");
    button.set_tooltip_text(Some(tooltip));
    button.set_valign(Align::Center);
    button
}

fn build(session: &str, card_title: &str, folder: &Path, origin: glib::WeakRef<gtk4::Button>) -> Drawer {
    let (outer, header) =
        crate::floating_panel::card_panel(&gtk4::Image::from_icon_name(attach_icon()), "Attachments", card_title, MIN_SIZE);
    outer.add_css_class("attach-panel");
    let close_button = header.close_button("Close; attached files are kept");

    let body = gtk4::Box::new(Orientation::Vertical, 8);
    body.set_vexpand(true);
    body.set_margin_top(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    outer.append(&body);

    // ---- attached so far ----
    let attached_head = gtk4::Box::new(Orientation::Horizontal, 8);
    let attached_title = gtk4::Label::new(None);
    attached_title.add_css_class("settings-entry-title");
    attached_title.set_hexpand(true);
    attached_title.set_xalign(0.0);
    attached_head.append(&attached_title);
    let clear = small_button("Clear", "Remove every attached file");
    attached_head.append(&clear);
    body.append(&attached_head);
    let attached_scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .min_content_height(64)
        .max_content_height(190)
        .propagate_natural_height(true)
        .build();
    let attached = gtk4::Box::new(Orientation::Vertical, 4);
    attached.add_css_class("attach-list");
    attached_scroll.set_child(Some(&attached));
    body.append(&attached_scroll);

    // ---- this PC's folders ----
    let browse_title = gtk4::Label::new(Some("Add files from this PC"));
    browse_title.add_css_class("settings-entry-title");
    browse_title.set_xalign(0.0);
    browse_title.set_margin_top(6);
    body.append(&browse_title);
    let nav = gtk4::Box::new(Orientation::Horizontal, 6);
    let up = small_button("↑", "Parent folder");
    nav.append(&up);
    let project = small_button("Project", "The folder this card runs in");
    nav.append(&project);
    let home = small_button("⌂ Home", "Your home folder");
    nav.append(&home);
    let place = gtk4::Label::new(None);
    place.add_css_class("attach-path");
    place.set_hexpand(true);
    place.set_xalign(0.0);
    place.set_ellipsize(gtk4::pango::EllipsizeMode::Start);
    nav.append(&place);
    let hidden = gtk4::CheckButton::with_label("Hidden");
    hidden.set_tooltip_text(Some("Show hidden files and folders"));
    nav.append(&hidden);
    body.append(&nav);
    let browser_scroll = gtk4::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .min_content_height(120)
        .build();
    let browser = gtk4::Box::new(Orientation::Vertical, 1);
    browser.add_css_class("attach-browser");
    browser_scroll.set_child(Some(&browser));
    body.append(&browser_scroll);
    let browse_note = gtk4::Label::new(None);
    browse_note.add_css_class("launcher-hint");
    browse_note.set_xalign(0.0);
    browse_note.set_wrap(true);
    body.append(&browse_note);

    // ---- footer ----
    let footer = gtk4::Box::new(Orientation::Horizontal, 10);
    footer.add_css_class("attach-footer");
    let status = gtk4::Label::new(Some("Drop files here or paste them with Ctrl+V."));
    status.add_css_class("launcher-hint");
    status.set_hexpand(true);
    status.set_xalign(0.0);
    status.set_wrap(true);
    footer.append(&status);
    let insert = gtk4::Button::with_label("Insert into prompt");
    insert.add_css_class("launcher-btn");
    insert.add_css_class("launcher-btn-primary");
    insert.set_tooltip_text(Some("Type the files into the harness's prompt, without sending it"));
    insert.set_valign(Align::Center);
    footer.append(&insert);
    outer.append(&footer);

    let session = session.to_string();
    let generation = Rc::new(Cell::new(0u64));
    let cwd = Rc::new(RefCell::new(folder.to_path_buf()));
    // The browser's file rows, for their ✓.
    let rows: Rc<RefCell<Vec<(PathBuf, gtk4::Button)>>> = Rc::new(RefCell::new(Vec::new()));

    // The ✕ on an attached row repaints through a weak handle on `paint`.
    let paint_slot: Rc<RefCell<Option<std::rc::Weak<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let paint: Rc<dyn Fn()> = Rc::new({
        let paint_slot = Rc::clone(&paint_slot);
        let session = session.clone();
        let attached = attached.clone();
        let attached_title = attached_title.clone();
        let clear = clear.clone();
        let insert = insert.clone();
        let rows = Rc::clone(&rows);
        move || {
            let files = staged(&session);
            attached_title.set_text(&match files.len() {
                0 => "Attached".to_string(),
                n => format!("Attached ({n} of {MAX_LOCAL_ATTACHMENTS})"),
            });
            clear.set_sensitive(!files.is_empty());
            insert.set_sensitive(!files.is_empty());
            while let Some(child) = attached.first_child() {
                attached.remove(&child);
            }
            if files.is_empty() {
                let empty = gtk4::Label::new(Some(
                    "Nothing attached yet. Pick files below, drop them here from a file manager, or paste copied files with Ctrl+V.",
                ));
                empty.add_css_class("launcher-hint");
                empty.set_wrap(true);
                empty.set_xalign(0.0);
                attached.append(&empty);
            }
            for path in &files {
                let row = gtk4::Box::new(Orientation::Horizontal, 8);
                row.add_css_class("attach-row");
                row.append(&gtk4::Label::new(Some(file_icon(path))));
                let words = gtk4::Box::new(Orientation::Vertical, 1);
                words.set_hexpand(true);
                let name = gtk4::Label::new(path.file_name().map(|n| n.to_string_lossy()).as_deref());
                name.add_css_class("attach-name");
                name.set_xalign(0.0);
                name.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
                words.append(&name);
                let detail = gtk4::Label::new(Some(&describe(path)));
                detail.add_css_class("launcher-subtitle");
                detail.set_xalign(0.0);
                detail.set_ellipsize(gtk4::pango::EllipsizeMode::Start);
                words.append(&detail);
                row.append(&words);
                let remove = gtk4::Button::with_label("✕");
                remove.add_css_class("term-btn");
                remove.set_tooltip_text(Some("Remove from the attachments"));
                remove.set_valign(Align::Center);
                let session = session.clone();
                let path = path.clone();
                let slot = Rc::clone(&paint_slot);
                remove.connect_clicked(move |_| {
                    let mut files = staged(&session);
                    files.retain(|f| f != &path);
                    set_staged(&session, files);
                    if let Some(paint) = slot.borrow().as_ref().and_then(std::rc::Weak::upgrade) {
                        paint();
                    }
                });
                row.append(&remove);
                attached.append(&row);
            }
            for (path, row) in rows.borrow().iter() {
                if files.contains(path) {
                    row.add_css_class("attached");
                } else {
                    row.remove_css_class("attached");
                }
            }
        }
    });
    *paint_slot.borrow_mut() = Some(Rc::downgrade(&paint));
    let repaint: Rc<dyn Fn()> = Rc::new(move || {
        if let Some(paint) = paint_slot.borrow().as_ref().and_then(std::rc::Weak::upgrade) {
            paint();
        }
    });

    let add: Rc<dyn Fn(Vec<PathBuf>)> = Rc::new({
        let session = session.clone();
        let status = status.clone();
        let repaint = Rc::clone(&repaint);
        move |new: Vec<PathBuf>| {
            let mut files = staged(&session);
            let before = files.len();
            let over = add_paths(&mut files, new);
            let added = files.len() - before;
            set_staged(&session, files);
            status.set_text(&if over > 0 {
                format!("At most {MAX_LOCAL_ATTACHMENTS} files at a time; {over} left out.")
            } else if added == 0 {
                "Already attached.".to_string()
            } else {
                "Insert them into the prompt when you are ready.".to_string()
            });
            repaint();
        }
    });

    let browse: Rc<dyn Fn(PathBuf)> = {
        let generation = Rc::clone(&generation);
        let cwd = Rc::clone(&cwd);
        let hidden = hidden.clone();
        let rows = Rc::clone(&rows);
        let session = session.clone();
        let add = Rc::clone(&add);
        let repaint = Rc::clone(&repaint);
        let status = status.clone();
        let up = up.clone();
        let place = place.clone();
        let browser = browser.clone();
        let browse_note = browse_note.clone();
        let slot: Rc<RefCell<Option<std::rc::Weak<dyn Fn(PathBuf)>>>> = Rc::new(RefCell::new(None));
        let browse: Rc<dyn Fn(PathBuf)> = Rc::new({
            let slot = Rc::clone(&slot);
            move |target: PathBuf| {
                generation.set(generation.get() + 1);
                let ticket = generation.get();
                *cwd.borrow_mut() = target.clone();
                place.set_text(&crate::state::display_dir(&target.to_string_lossy()));
                place.set_tooltip_text(Some(&target.to_string_lossy()));
                up.set_sensitive(target.parent().is_some());
                browse_note.set_text("Loading…");
                let show_hidden = hidden.is_active();
                let generation = Rc::clone(&generation);
                let browser = browser.clone();
                let browse_note = browse_note.clone();
                let rows = Rc::clone(&rows);
                let session = session.clone();
                let add = Rc::clone(&add);
                let repaint = Rc::clone(&repaint);
                let status = status.clone();
                let slot = Rc::clone(&slot);
                glib::MainContext::default().spawn_local(async move {
                    let folder = target.clone();
                    let listing = gio::spawn_blocking(move || list_folder(&folder, show_hidden)).await;
                    if generation.get() != ticket {
                        return;
                    }
                    while let Some(child) = browser.first_child() {
                        browser.remove(&child);
                    }
                    rows.borrow_mut().clear();
                    let (entries, cut) = match listing {
                        Ok(Ok(listing)) => listing,
                        _ => {
                            browse_note.set_text("This folder cannot be opened.");
                            return;
                        }
                    };
                    browse_note.set_text(&match (entries.len(), cut) {
                        (0, _) => "This folder is empty.".to_string(),
                        (_, true) => format!("Showing the first {LIST_CAP} entries. Click a file to attach it, a folder to open it."),
                        _ => "Click a file to attach it, a folder to open it.".to_string(),
                    });
                    let attached_now = staged(&session);
                    for entry in entries {
                        let row = gtk4::Button::new();
                        row.add_css_class("attach-browse-row");
                        let line = gtk4::Box::new(Orientation::Horizontal, 8);
                        line.append(&gtk4::Label::new(Some(file_icon(&entry.path))));
                        let name = gtk4::Label::new(Some(&entry.name));
                        name.set_hexpand(true);
                        name.set_xalign(0.0);
                        name.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
                        line.append(&name);
                        let meta = gtk4::Label::new(Some(&if entry.is_dir {
                            "›".to_string()
                        } else {
                            glib::format_size(entry.size).to_string()
                        }));
                        meta.add_css_class("launcher-subtitle");
                        line.append(&meta);
                        if !entry.is_dir {
                            let check = gtk4::Label::new(Some("✓"));
                            check.add_css_class("attach-check");
                            line.append(&check);
                        }
                        row.set_child(Some(&line));
                        if entry.is_dir {
                            row.set_tooltip_text(Some("Open this folder"));
                            let slot = Rc::clone(&slot);
                            let path = entry.path.clone();
                            row.connect_clicked(move |_| {
                                if let Some(browse) = slot.borrow().as_ref().and_then(std::rc::Weak::upgrade) {
                                    browse(path.clone());
                                }
                            });
                        } else {
                            row.set_tooltip_text(Some("Attach or remove this file"));
                            if attached_now.contains(&entry.path) {
                                row.add_css_class("attached");
                            }
                            let session = session.clone();
                            let path = entry.path.clone();
                            let add = Rc::clone(&add);
                            let repaint = Rc::clone(&repaint);
                            let status = status.clone();
                            row.connect_clicked(move |_| {
                                let mut files = staged(&session);
                                if files.contains(&path) {
                                    files.retain(|f| f != &path);
                                    set_staged(&session, files);
                                    status.set_text("Removed.");
                                    repaint();
                                } else {
                                    add(vec![path.clone()]);
                                }
                            });
                            rows.borrow_mut().push((entry.path.clone(), row.clone()));
                        }
                        browser.append(&row);
                    }
                });
            }
        });
        *slot.borrow_mut() = Some(Rc::downgrade(&browse));
        browse
    };

    // Navigation.
    {
        let browse = Rc::clone(&browse);
        let cwd = Rc::clone(&cwd);
        up.connect_clicked(move |_| {
            let parent = cwd.borrow().parent().map(Path::to_path_buf);
            if let Some(parent) = parent {
                browse(parent);
            }
        });
    }
    {
        let browse = Rc::clone(&browse);
        let folder = folder.to_path_buf();
        project.connect_clicked(move |_| browse(folder.clone()));
    }
    {
        let browse = Rc::clone(&browse);
        home.connect_clicked(move |_| browse(crate::state::home_dir()));
    }
    {
        let browse = Rc::clone(&browse);
        let cwd = Rc::clone(&cwd);
        hidden.connect_toggled(move |_| {
            let here = cwd.borrow().clone();
            browse(here);
        });
    }
    {
        let session = session.clone();
        let status = status.clone();
        let repaint = Rc::clone(&repaint);
        clear.connect_clicked(move |_| {
            set_staged(&session, Vec::new());
            status.set_text("Cleared.");
            repaint();
        });
    }

    // Drop files from a file manager anywhere on the panel.
    let drop = gtk4::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    {
        let panel = outer.clone();
        drop.connect_enter(move |_, _, _| {
            panel.add_css_class("attach-drop-hover");
            gdk::DragAction::COPY
        });
        let panel = outer.clone();
        drop.connect_leave(move |_| panel.remove_css_class("attach-drop-hover"));
        let panel = outer.clone();
        let add = Rc::clone(&add);
        drop.connect_drop(move |_, value, _, _| {
            panel.remove_css_class("attach-drop-hover");
            let Ok(list) = value.get::<gdk::FileList>() else { return false };
            let paths: Vec<PathBuf> = list.files().iter().filter_map(gio::prelude::FileExt::path).collect();
            if paths.is_empty() {
                return false;
            }
            add(paths);
            true
        });
    }
    outer.add_controller(drop);

    // Ctrl+V pastes files copied in a file manager.
    let keys = gtk4::EventControllerKey::new();
    {
        let add = Rc::clone(&add);
        let status = status.clone();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            if !(modifiers.contains(gdk::ModifierType::CONTROL_MASK) && matches!(key, gdk::Key::v | gdk::Key::V)) {
                return glib::Propagation::Proceed;
            }
            let Some(display) = gdk::Display::default() else { return glib::Propagation::Proceed };
            let clipboard = display.clipboard();
            let add = Rc::clone(&add);
            let status = status.clone();
            glib::MainContext::default().spawn_local(async move {
                let value = clipboard.read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT).await;
                let paths: Vec<PathBuf> = value
                    .ok()
                    .and_then(|value| value.get::<gdk::FileList>().ok())
                    .map(|list| list.files().iter().filter_map(gio::prelude::FileExt::path).collect())
                    .unwrap_or_default();
                if paths.is_empty() {
                    status.set_text("The clipboard holds no files.");
                } else {
                    add(paths);
                }
            });
            glib::Propagation::Stop
        });
    }
    outer.add_controller(keys);

    // Insert: type the files into the harness's prompt, then hand the
    // keyboard to its terminal so the user can finish the prompt.
    {
        let session = session.clone();
        let status = status.clone();
        insert.connect_clicked(move |button| {
            let files = staged(&session);
            if files.is_empty() {
                return;
            }
            button.set_sensitive(false);
            status.set_text("Inserting…");
            let session = session.clone();
            let status = status.clone();
            let button = button.clone();
            let origin = origin.clone();
            glib::MainContext::default().spawn_local(async move {
                let target = session.clone();
                let sent = files.clone();
                let result = gio::spawn_blocking(move || crate::prompt_attachments::insert_local(&target, &sent))
                    .await
                    .unwrap_or_else(|_| Err("insert_failed".into()));
                match result {
                    Ok(_) => {
                        // Files added while this ran stay attached.
                        let mut left = staged(&session);
                        left.retain(|f| !files.contains(f));
                        let done = left.is_empty();
                        set_staged(&session, left);
                        if done {
                            close(&session);
                        } else {
                            status.set_text("Inserted. Files added meanwhile are still attached.");
                            button.set_sensitive(true);
                        }
                        if let Some(origin) = origin.upgrade() {
                            focus_card_terminal(&origin);
                        }
                    }
                    Err(error) => {
                        status.set_text(&explain(&error));
                        button.set_sensitive(true);
                    }
                }
            });
        });
    }

    paint();
    browse(folder.to_path_buf());
    let widget = outer;
    Drawer { widget, close: close_button, generation, insert, status, paint, add, browse }
}

/// The panel's stylesheet, in the Omarchy palette.
pub fn css(theme: &OmarchyTheme) -> String {
    format!(
        r#"
/* ================= Card attachments (📎) ================= */
/* A small badge on the clip's corner, so a count adds no width. */
.attach-count {{
    font-size: 8px;
    font-weight: 800;
    min-width: 6px;
    padding: 0 2px;
    margin: -3px -4px 0 0;
    border-radius: 6px;
    color: {bg};
    background-color: {accent};
}}
.term-btn.attach-btn.has-attachments {{
    color: {accent};
    background-color: {badge_bg};
}}
.attach-panel.attach-drop-hover {{
    border: 2px dashed {accent};
}}
.attach-row {{
    padding: 6px 8px;
    border-radius: 8px;
    border: 1px solid {line};
    background-color: {row_bg};
}}
.attach-name {{
    color: {bright_fg};
    font-weight: 700;
}}
.attach-path {{
    color: {dim_fg};
    font-family: '{font}', monospace;
    font-size: 11px;
}}
.attach-browser {{
    padding: 2px;
}}
button.attach-browse-row {{
    background: none;
    border: none;
    box-shadow: none;
    border-radius: 6px;
    padding: 4px 8px;
    color: {fg};
}}
button.attach-browse-row:hover {{
    background-color: {badge_bg};
}}
button.attach-browse-row .attach-check {{
    color: transparent;
    font-weight: 800;
}}
button.attach-browse-row.attached {{
    background-color: {badge_bg};
    box-shadow: inset 2px 0 {accent};
}}
button.attach-browse-row.attached .attach-check {{
    color: {accent};
}}
.attach-footer {{
    padding: 10px 12px 12px 12px;
    border-top: 1px solid {line};
}}
"#,
        accent = theme.accent,
        bg = theme.background,
        fg = theme.foreground,
        bright_fg = theme.bright_foreground,
        dim_fg = theme.dark_foreground,
        font = theme.font_family,
        badge_bg = theme.rgba_accent(0.15),
        line = theme.rgba_muted(0.3),
        row_bg = theme.rgba_darker_bg(0.35),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attaching_skips_duplicates_and_stops_at_the_limit() {
        let mut files = vec![PathBuf::from("/a")];
        assert_eq!(add_paths(&mut files, [PathBuf::from("/a"), PathBuf::from("/b")]), 0);
        assert_eq!(files, [PathBuf::from("/a"), PathBuf::from("/b")]);
        let more = (0..MAX_LOCAL_ATTACHMENTS + 3).map(|i| PathBuf::from(format!("/f{i}")));
        assert_eq!(add_paths(&mut files, more), 5);
        assert_eq!(files.len(), MAX_LOCAL_ATTACHMENTS);
    }

    #[test]
    fn folders_list_folders_first_and_hide_dot_files() {
        let dir = std::env::temp_dir().join(format!("sd-attach-list-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        for name in ["b.txt", "A.png", ".env"] {
            std::fs::write(dir.join(name), b"12345").unwrap();
        }
        let names = |hidden| -> Vec<String> {
            list_folder(&dir, hidden).unwrap().0.into_iter().map(|e| e.name).collect()
        };
        assert_eq!(names(false), ["src", "A.png", "b.txt"]);
        assert_eq!(names(true), [".git", "src", ".env", "A.png", "b.txt"]);
        let (entries, cut) = list_folder(&dir, false).unwrap();
        assert!(!cut && entries[0].is_dir && entries[1].size == 5);
        assert!(list_folder(&dir.join("nope"), false).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn insert_errors_read_as_sentences() {
        assert_eq!(explain("missing:plan.md"), "plan.md no longer exists. Remove it and try again.");
        assert_eq!(explain("no_such_session"), "This terminal is no longer running.");
        assert!(explain("image_not_confirmed").contains("Check its prompt"));
        assert!(explain("something_else").contains("something_else"));
    }

    #[test]
    fn the_panel_attaches_from_its_folder_browser() {
        crate::gtk_test::run_in_child_process("attachment_view::tests::panel_inner");
    }

    /// With `SD_ATTACH_SHOTS=<dir>` it also saves a picture of the panel.
    #[test]
    fn panel_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        gtk4::Settings::default().unwrap().set_gtk_application_prefer_dark_theme(true);
        crate::styles::apply_styles();
        let dir = std::env::temp_dir().join(format!("sd-attach-panel-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("design.md"), b"# plan").unwrap();
        std::fs::write(dir.join("screenshot.png"), b"png").unwrap();
        let session = "sd_term_attach_test";
        let card_button = button(session.into(), "Claude Code".into(), dir.to_string_lossy().into_owned());
        let count = count_label(&card_button).unwrap();
        assert!(!count.is_visible(), "no count while nothing is attached");
        assert!(!card_button.has_css_class("has-attachments"));
        let drawer = build(session, "Claude Code · ~/app", &dir, card_button.downgrade());
        let window = gtk4::Window::new();
        window.set_default_size(DEFAULT_SIZE.0, DEFAULT_SIZE.1);
        window.set_child(Some(&drawer.widget));
        window.present();

        let rows = |class: &str| crate::gtk_test::find_all::<gtk4::Widget>(drawer.widget.upcast_ref(), class);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while rows("attach-browse-row").len() < 3 {
            assert!(std::time::Instant::now() < until, "the folder never listed");
            crate::gtk_test::pump(20);
        }
        assert!(!drawer.insert.is_sensitive(), "nothing to insert yet");
        // Click a file: attached, counted on the card's button, ✓ in the browser.
        let file_row = |name: &str| {
            rows("attach-browse-row").into_iter()
                .map(|w| w.downcast::<gtk4::Button>().unwrap())
                .find(|b| b.tooltip_text().as_deref() == Some("Attach or remove this file")
                    && {
                        let mut labels = Vec::new();
                        let mut child = b.child().unwrap().first_child();
                        while let Some(c) = child { child = c.next_sibling(); if let Ok(l) = c.downcast::<gtk4::Label>() { labels.push(l.text().to_string()); } }
                        labels.iter().any(|l| l == name)
                    })
                .unwrap()
        };
        file_row("design.md").emit_clicked();
        assert_eq!(staged(session), [dir.join("design.md")]);
        assert!(card_button.has_css_class("has-attachments"));
        assert!(count.is_visible() && count.text() == "1");
        assert!(file_row("design.md").has_css_class("attached"));
        assert!(drawer.insert.is_sensitive());
        // A drop or a paste adds more; duplicates are ignored.
        (drawer.add)(vec![dir.join("screenshot.png"), dir.join("design.md")]);
        assert_eq!(staged(session).len(), 2);
        assert_eq!(rows("attach-row").len(), 2);
        // Clicking an attached file again removes it.
        file_row("design.md").emit_clicked();
        assert_eq!(staged(session), [dir.join("screenshot.png")]);
        assert_eq!(rows("attach-row").len(), 1);
        crate::gtk_test::pump(100);
        if let Some(out) = std::env::var_os("SD_ATTACH_SHOTS").map(PathBuf::from) {
            (drawer.add)(vec![dir.join("design.md"), dir.join("src")]);
            crate::gtk_test::pump(200);
            std::fs::create_dir_all(&out).unwrap();
            crate::gtk_test::save_png(&drawer.widget, &out.join("attachments.png"));
            set_staged(session, vec![dir.join("screenshot.png")]);
            (drawer.paint)();
        }
        // The ✕ on an attached row removes it.
        let remove = rows("attach-row")[0].last_child().unwrap().downcast::<gtk4::Button>().unwrap();
        remove.emit_clicked();
        assert!(staged(session).is_empty());
        assert!(!card_button.has_css_class("has-attachments"));
        assert!(!count.is_visible());
        assert!(!drawer.insert.is_sensitive());
        // A folder opens in the browser.
        (drawer.browse)(dir.join("src"));
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !rows("attach-path").iter().any(|w| w.downcast_ref::<gtk4::Label>().unwrap().text().ends_with("/src")) {
            assert!(std::time::Instant::now() < until, "the subfolder never opened");
            crate::gtk_test::pump(20);
        }
        // A file deleted after it was attached: Insert says so before it
        // reads anything else, and keeps the list.
        std::fs::write(dir.join("gone.md"), b"x").unwrap();
        (drawer.add)(vec![dir.join("gone.md")]);
        std::fs::remove_file(dir.join("gone.md")).unwrap();
        drawer.insert.emit_clicked();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while drawer.status.text() == "Inserting…" || !drawer.insert.is_sensitive() {
            assert!(std::time::Instant::now() < until, "insert never answered");
            crate::gtk_test::pump(20);
        }
        assert_eq!(drawer.status.text(), "gone.md no longer exists. Remove it and try again.");
        assert_eq!(staged(session).len(), 1);
        window.close();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn panel_opened_from_the_card_lists_clicked_files() {
        crate::gtk_test::run_in_child_process("attachment_view::tests::opened_panel_inner");
    }

    /// Opened the way the app opens it, from the card's 📎: only the panel
    /// itself keeps it alive, and a clicked file still shows in its list.
    #[test]
    fn opened_panel_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let dir = std::env::temp_dir().join(format!("sd-attach-opened-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), b"x").unwrap();
        let overlay = gtk4::Overlay::new();
        overlay.set_child(Some(&gtk4::Box::new(Orientation::Vertical, 0)));
        let ceiling = gtk4::Box::new(Orientation::Vertical, 0);
        overlay.add_overlay(&ceiling);
        crate::asset_view::set_host(&overlay, Rc::new(|| 0), &ceiling);
        let window = gtk4::Window::new();
        window.set_default_size(900, 700);
        window.set_child(Some(&overlay));
        window.present();
        let session = "sd_term_attach_opened";
        let card_button = button(session.into(), "Claude Code".into(), dir.to_string_lossy().into_owned());
        card_button.emit_clicked();
        let find = |class: &str| crate::gtk_test::find_all::<gtk4::Widget>(overlay.upcast_ref(), class);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while find("attach-browse-row").is_empty() {
            assert!(std::time::Instant::now() < until, "the folder never listed");
            crate::gtk_test::pump(20);
        }
        let row = find("attach-browse-row").pop().unwrap().downcast::<gtk4::Button>().unwrap();
        row.emit_clicked();
        assert_eq!(staged(session), [dir.join("notes.md")]);
        assert_eq!(find("attach-row").len(), 1, "the clicked file is listed");
        assert!(row.has_css_class("attached"), "and checked in the browser");
        // Closing drops the panel and its repaint; the file stays attached.
        let close_button = find("attach-panel").pop().unwrap()
            .first_child().unwrap().last_child().unwrap().downcast::<gtk4::Button>().unwrap();
        close_button.emit_clicked();
        assert!(find("attach-panel").is_empty());
        assert_eq!(PANELS.with(crate::floating_panel::CardPanels::len), 0);
        assert_eq!(staged(session).len(), 1);
        set_staged(session, Vec::new());
        window.close();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn card_header_holds_the_attach_button() {
        crate::gtk_test::run_in_child_process("attachment_view::tests::card_header_inner");
    }

    /// 📎 sits in a local card's header between Files and history, never in
    /// its footer, and the header still fits a default-size card with every
    /// status and a count showing: the header sets the card's minimum width.
    #[test]
    fn card_header_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let session = "sd_term_header";
        let data = crate::state::TerminalData {
            id: session.into(), session_name: session.into(),
            agent_type: "claude".into(), command: "claude".into(),
            x: 0, y: 0, width: 520, height: 300, restored_width: 520, restored_height: 300,
            iconified: true, icon_x: None, icon_y: None, created_at: 0.0, tag: 0,
            agent_session_id: None, workspace_dir: Some("/tmp".into()),
        };
        let card = crate::mini_terminal::MiniTerminalCard::new(
            data, |_, _, _| {}, |_, _| {}, |_| {}, |_| {}, |_, _, _, _, _| {}, || {}, |_| {}, |_| {}, || {},
            1024, 768, None, Some(Rc::new(Vec::new())),
            crate::mini_terminal::HoverRaiseLock::new(), crate::card_source::CardSource::Local,
        );
        card.open_with_bare_terminal(520, 300);
        let find = crate::gtk_test::find_all::<gtk4::Widget>;
        let root = card.container.clone().upcast::<gtk4::Widget>();
        let header = find(&root, "term-header").pop().expect("a header");
        let footer = find(&root, "term-footer").pop().expect("a footer");
        assert!(find(&footer, "attach-btn").is_empty(), "📎 left the footer");
        let panels = find(&header, "term-panel-btns").pop().expect("panel buttons in the header");
        let labels: Vec<String> = std::iter::successors(panels.first_child(), |w| w.next_sibling())
            .map(|w| w.downcast::<gtk4::Button>().unwrap())
            .map(|b| b.tooltip_text().unwrap_or_default().to_string())
            .collect();
        assert_eq!(labels.len(), 3, "{labels:?}");
        assert!(labels[0].starts_with("Files"), "{labels:?}");
        assert_eq!(labels[1], "Attach files to this harness");

        set_staged(session, vec![PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.md")]);
        let button = find(&header, "attach-btn").pop().unwrap().downcast::<gtk4::Button>().unwrap();
        assert_eq!(count_label(&button).unwrap().text(), "2");
        let badge = find(&header, "term-status-badge").pop().unwrap().downcast::<gtk4::Label>().unwrap();
        for status in ["● IDLE", "○ EXITED", "● WORKING"] {
            badge.set_label(status);
            // Its border puts the card a pixel or two outside the header.
            let width = header.measure(Orientation::Horizontal, -1).0;
            assert!(
                width + 4 <= crate::mini_terminal::CARD_WIDTH,
                "{status}: the header needs {width}px, a default card is {}px",
                crate::mini_terminal::CARD_WIDTH
            );
        }
        set_staged(session, Vec::new());
    }

    /// A picture of a card with its 📎, for a visual check:
    /// `SD_ATTACH_SHOTS=<dir> cargo test attachment_view::tests::card_shot`.
    #[test]
    fn card_shot() {
        if std::env::var_os("SD_ATTACH_SHOTS").is_none() {
            return;
        }
        crate::gtk_test::run_in_child_process("attachment_view::tests::card_shot_inner");
    }

    #[test]
    fn card_shot_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let Some(out) = std::env::var_os("SD_ATTACH_SHOTS").map(PathBuf::from) else { return };
        gtk4::init().unwrap();
        gtk4::Settings::default().unwrap().set_gtk_application_prefer_dark_theme(true);
        crate::styles::apply_styles();
        let data = crate::state::TerminalData {
            id: "sd_term_shot".into(), session_name: "sd_term_shot".into(),
            agent_type: "claude".into(), command: "claude".into(),
            x: 0, y: 0, width: 520, height: 300, restored_width: 520, restored_height: 300,
            iconified: true, icon_x: None, icon_y: None, created_at: 0.0, tag: 0,
            agent_session_id: None, workspace_dir: Some("/tmp".into()),
        };
        set_staged("sd_term_shot", vec![PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.md")]);
        let card = Rc::new(crate::mini_terminal::MiniTerminalCard::new(
            data, |_, _, _| {}, |_, _| {}, |_| {}, |_| {}, |_, _, _, _, _| {}, || {}, |_| {}, |_| {}, || {},
            1024, 768, None, Some(Rc::new(Vec::new())),
            crate::mini_terminal::HoverRaiseLock::new(), crate::card_source::CardSource::Local,
        ));
        card.open_with_bare_terminal(520, 300);
        let window = gtk4::Window::new();
        window.set_default_size(560, 340);
        let canvas = gtk4::Fixed::new();
        canvas.put(&card.container, 20.0, 20.0);
        window.set_child(Some(&canvas));
        window.present();
        crate::gtk_test::pump(400);
        std::fs::create_dir_all(&out).unwrap();
        crate::gtk_test::save_png(&card.container, &out.join("card.png"));
    }
}
