//! SD Editor: a text editor card on the desktop, next to the terminals. Its
//! folder's tree is on the left, the open files in tabs on the right, drawn
//! by GtkSourceView (syntax colors from the Omarchy theme, line numbers,
//! find and replace, undo). Images, sound and video open in preview tabs. It drags, resizes, arranges and slides like any
//! card. Questions (save changes? a file name) are asked inside the card:
//! the overlay cannot show system dialogs.
//!
//! Which editors are open, where, and with which files is kept in
//! `editors.json` next to state.json, never in the workspace sent to phones
//! or other PCs.
mod doc;
mod find;
mod media;
mod scheme;
mod tree;

pub use doc::Doc;

use crate::plugin_ui::cards::{CanvasHost, Geometry, PluginCard};
use gtk4::prelude::*;
use gtk4::{gio, glib};
use sourceview5::prelude::*;
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

const DEFAULT_SIZE: (i32, i32) = (980, 620);
/// The CSS class of an editor's body.
const CLASS: &str = "sd-editor";

thread_local! {
    static HOST: RefCell<Option<Rc<CanvasHost>>> = const { RefCell::new(None) };
    /// The top bar's folder, where ✎ opens SD Editor.
    static TOP_BAR_FOLDER: RefCell<Option<Rc<dyn Fn() -> PathBuf>>> = const { RefCell::new(None) };
    static BUTTON: RefCell<Option<glib::WeakRef<gtk4::Button>>> = const { RefCell::new(None) };
    static EDITORS: RefCell<Vec<Rc<Editor>>> = const { RefCell::new(Vec::new()) };
    static RESTORING: Cell<bool> = const { Cell::new(false) };
}

/// The canvas editors go on (the window's, shared with plugin cards).
pub fn set_host(host: Rc<CanvasHost>) {
    HOST.with(|cell| *cell.borrow_mut() = Some(host));
}

/// SD Editor's icon: a high-voltage bolt with a pencil (assets/icons).
pub const ICON: &str = "sd-editor";

/// Whether the icon theme has SD Editor's icon (an installed copy of the
/// assets older than it does not).
fn has_icon() -> bool {
    gtk4::gdk::Display::default().is_some_and(|display| gtk4::IconTheme::for_display(&display).has_icon(ICON))
}

/// The top bar's SD Editor button: opens SD Editor on the folder chosen in
/// the top bar (`folder`). Shown unless turned off in Settings → SD Editor.
pub fn toolbar_button(folder: Rc<dyn Fn() -> PathBuf>, shown: bool) -> gtk4::Button {
    TOP_BAR_FOLDER.with(|cell| *cell.borrow_mut() = Some(folder));
    let button = if has_icon() {
        let button = gtk4::Button::from_icon_name(ICON);
        button.add_css_class("hud-icon-btn");
        if let Some(image) = button.child().and_downcast::<gtk4::Image>() {
            image.set_pixel_size(22);
        }
        button
    } else {
        gtk4::Button::with_label("✎")
    };
    button.add_css_class("hud-button");
    button.add_css_class("sd-editor-button");
    button.set_tooltip_text(Some("SD Editor: edit the files of the selected folder"));
    button.update_property(&[gtk4::accessible::Property::Label("SD Editor")]);
    button.set_visible(shown);
    button.connect_clicked(|button| {
        if let Err(e) = open_here() {
            button.set_tooltip_text(Some(&format!("SD Editor: {e}")));
        }
    });
    BUTTON.with(|cell| *cell.borrow_mut() = Some(button.downgrade()));
    button
}

/// Shows or hides the top bar's ✎ (Settings → SD Editor).
pub fn set_button_shown(shown: bool) {
    if let Some(button) = BUTTON.with(|cell| cell.borrow().as_ref().and_then(|b| b.upgrade())) {
        button.set_visible(shown);
    }
}

/// Opens SD Editor on the folder chosen in the top bar.
pub fn open_here() -> Result<(), String> {
    let folder = TOP_BAR_FOLDER.with(|cell| cell.borrow().clone()).ok_or("the desktop is not ready")?;
    open(&folder(), None)
}

fn host() -> Option<Rc<CanvasHost>> {
    HOST.with(|cell| cell.borrow().clone())
}

fn editors() -> Vec<Rc<Editor>> {
    EDITORS.with(|cell| cell.borrow().clone())
}

/// The open editors' cards, for Arrange, the slide and display changes.
pub fn cards() -> Vec<Rc<PluginCard>> {
    editors().iter().map(|e| Rc::clone(&e.card)).collect()
}

/// The Omarchy theme changed: new colors for every open file.
pub fn theme_changed() {
    let scheme = scheme::current();
    for editor in editors() {
        for doc in editor.docs.borrow().iter() {
            doc.buffer.set_style_scheme(scheme.as_ref());
        }
    }
}

/// Opens an editor on `folder` (or brings the one already there to the
/// front) and opens `file` in it, a path in `folder` or absolute.
pub fn open(folder: &Path, file: Option<&Path>) -> Result<(), String> {
    let host = host().ok_or("the desktop is not ready")?;
    let folder = folder.canonicalize().map_err(|e| format!("{}: {e}", folder.display()))?;
    if !folder.is_dir() {
        return Err(format!("{} is not a folder", folder.display()));
    }
    let editor = match editors().into_iter().find(|e| e.root == folder) {
        Some(editor) => editor,
        None => {
            let step = 32 * editors().len().min(8) as i32;
            let start = crate::plugin_ui::cards::initial_geometry(&host, None, DEFAULT_SIZE);
            let geometry = crate::plugin_ui::cards::initial_geometry(&host, Some(Geometry { x: start.x + step, y: start.y + step, ..start }), DEFAULT_SIZE);
            let editor = Editor::new(&host, &new_id(), &folder, geometry, false);
            persist();
            editor
        }
    };
    (host.raise)(editor.card.container.upcast_ref());
    match file {
        Some(file) => editor.open_file(&editor.resolve(&file.to_string_lossy())),
        None => editor.focus(),
    }
    Ok(())
}

fn new_id() -> String {
    let taken: Vec<String> = editors().iter().map(|e| e.id.clone()).collect();
    (1..).map(|n| format!("editor-{n}")).find(|id| !taken.contains(id)).unwrap_or_default()
}

// ── saved editors ─────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    #[serde(default)]
    editors: Vec<SavedEditor>,
}

#[derive(Serialize, Deserialize, Clone)]
struct SavedEditor {
    id: String,
    root: PathBuf,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    #[serde(default)]
    files: Vec<PathBuf>,
    #[serde(default)]
    active: usize,
    #[serde(default)]
    show_hidden: bool,
}

fn saved_path() -> PathBuf {
    crate::state::get_state_path().with_file_name("editors.json")
}

/// Writes which editors are open, where and with which files.
fn persist() {
    if RESTORING.with(Cell::get) {
        return;
    }
    let saved = Saved { editors: editors().iter().map(|e| e.saved()).collect() };
    if let Ok(text) = serde_json::to_string_pretty(&saved) {
        let _ = std::fs::write(saved_path(), text);
    }
}

/// Opens the editors that were open when SUPER DESKTOP last stopped. A
/// folder that is gone is skipped, and so is a file.
pub fn restore() {
    let Some(host) = host() else { return };
    let saved: Saved = std::fs::read_to_string(saved_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    RESTORING.with(|r| r.set(true));
    for entry in saved.editors {
        if !entry.root.is_dir() || editors().iter().any(|e| e.id == entry.id || e.root == entry.root) {
            continue;
        }
        let geometry = crate::plugin_ui::cards::initial_geometry(&host, Some(Geometry { x: entry.x, y: entry.y, width: entry.width, height: entry.height }), DEFAULT_SIZE);
        let editor = Editor::new(&host, &entry.id, &entry.root, geometry, entry.show_hidden);
        for file in entry.files.iter().filter(|f| f.is_file()) {
            editor.open_file(file);
        }
        editor.notebook.set_current_page(Some(entry.active.min((editor.notebook.n_pages() as usize).saturating_sub(1)) as u32));
    }
    RESTORING.with(|r| r.set(false));
}

// ── the questions asked inside the card ──────────────────────────────────

enum Answer {
    /// The main button, with the text typed (when there is a field).
    Yes(String),
    /// The second button ("Don't save").
    Other,
    No,
}

type OnAnswer = Box<dyn Fn(Answer)>;

struct Prompt {
    widget: gtk4::Revealer,
    question: gtk4::Label,
    entry: gtk4::Entry,
    yes: gtk4::Button,
    other: gtk4::Button,
    no: gtk4::Button,
    answer: RefCell<Option<OnAnswer>>,
}

impl Prompt {
    fn new() -> Rc<Self> {
        let question = gtk4::Label::new(None);
        question.set_xalign(0.0);
        question.set_wrap(true);
        let entry = gtk4::Entry::new();
        entry.set_hexpand(true);
        let yes = gtk4::Button::new();
        yes.add_css_class("suggested-action");
        let other = gtk4::Button::new();
        other.add_css_class("destructive-action");
        let no = gtk4::Button::with_label("Cancel");
        let buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        buttons.set_halign(gtk4::Align::End);
        buttons.append(&no);
        buttons.append(&other);
        buttons.append(&yes);
        let column = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
        column.add_css_class("sd-editor-bar");
        column.add_css_class("sd-editor-prompt");
        column.append(&question);
        column.append(&entry);
        column.append(&buttons);
        let widget = gtk4::Revealer::new();
        widget.set_child(Some(&column));
        widget.set_transition_type(gtk4::RevealerTransitionType::SlideDown);
        let prompt = Rc::new(Self { widget, question, entry, yes, other, no, answer: RefCell::new(None) });
        let answer_with = |prompt: &Rc<Self>, make: fn(&Self) -> Answer| {
            let weak = Rc::downgrade(prompt);
            move || {
                if let Some(prompt) = weak.upgrade() {
                    prompt.answer(make(&prompt));
                }
            }
        };
        let yes = answer_with(&prompt, |p| Answer::Yes(p.entry.text().trim().to_owned()));
        let other = answer_with(&prompt, |_| Answer::Other);
        let no = answer_with(&prompt, |_| Answer::No);
        prompt.yes.connect_clicked({ let yes = yes.clone(); move |_| yes() });
        prompt.entry.connect_activate(move |_| yes());
        prompt.other.connect_clicked(move |_| other());
        prompt.no.connect_clicked(move |_| no());
        prompt
    }

    fn is_open(&self) -> bool {
        self.widget.reveals_child()
    }

    /// Asks `question`: a field holding `text` when given, the main button
    /// `yes`, and `other` beside Cancel when given.
    fn ask(&self, question: &str, text: Option<&str>, yes: &str, other: Option<&str>, answer: impl Fn(Answer) + 'static) {
        self.question.set_text(question);
        self.entry.set_visible(text.is_some());
        self.entry.set_text(text.unwrap_or(""));
        self.yes.set_label(yes);
        self.other.set_visible(other.is_some());
        self.other.set_label(other.unwrap_or(""));
        *self.answer.borrow_mut() = Some(Box::new(answer));
        self.widget.set_reveal_child(true);
        if text.is_some() {
            self.entry.grab_focus();
            // The name, not the folder and extension, is what gets retyped.
            let typed = self.entry.text();
            let start = typed.rfind('/').map_or(0, |i| i + 1);
            let end = typed.rfind('.').filter(|&i| i > start).unwrap_or(typed.len());
            self.entry.select_region(typed[..start].chars().count() as i32, typed[..end].chars().count() as i32);
        } else {
            self.yes.grab_focus();
        }
    }

    fn answer(&self, answer: Answer) {
        self.widget.set_reveal_child(false);
        if let Some(callback) = self.answer.take() {
            callback(answer);
        }
    }
}

// ── the editor ───────────────────────────────────────────────────────────

pub struct Editor {
    id: String,
    root: PathBuf,
    pub card: Rc<PluginCard>,
    tree: Rc<tree::Tree>,
    stack: gtk4::Stack,
    notebook: gtk4::Notebook,
    docs: RefCell<Vec<Rc<Doc>>>,
    previews: RefCell<Vec<Rc<media::Preview>>>,
    prompt: Rc<Prompt>,
    find: Rc<find::Find>,
    status: gtk4::Label,
    position: gtk4::Label,
    untitled: Cell<u32>,
    /// Kept for the cursor-position handler of the open file.
    cursor_handler: RefCell<Option<(sourceview5::Buffer, glib::SignalHandlerId)>>,
}

fn pretty(path: &Path) -> String {
    let home = crate::state::home_dir_string();
    let text = path.display().to_string();
    match text.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => text,
    }
}

impl Editor {
    fn new(host: &Rc<CanvasHost>, id: &str, root: &Path, geometry: Geometry, show_hidden: bool) -> Rc<Self> {
        let body = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        body.add_css_class(CLASS);
        // Before anything inside it is made: a button made earlier would not
        // find a group added later.
        let actions = gio::SimpleActionGroup::new();
        body.insert_action_group("ed", Some(&actions));

        // File menu and the most used buttons.
        let menu = gio::Menu::new();
        let files = gio::Menu::new();
        files.append(Some("New file\tCtrl+N"), Some("ed.new"));
        files.append(Some("Open…\tCtrl+O"), Some("ed.open"));
        files.append(Some("Save\tCtrl+S"), Some("ed.save"));
        files.append(Some("Save as…\tCtrl+Shift+S"), Some("ed.save-as"));
        files.append(Some("Close file\tCtrl+W"), Some("ed.close-file"));
        menu.append_section(None, &files);
        let edit = gio::Menu::new();
        edit.append(Some("Find\tCtrl+F"), Some("ed.find"));
        edit.append(Some("Replace\tCtrl+H"), Some("ed.replace"));
        edit.append(Some("Go to line…\tCtrl+L"), Some("ed.goto"));
        menu.append_section(None, &edit);
        let view = gio::Menu::new();
        view.append(Some("Show hidden files"), Some("ed.hidden"));
        menu.append_section(None, &view);
        let close = gio::Menu::new();
        close.append(Some("Close editor"), Some("ed.close"));
        menu.append_section(None, &close);
        let file_button = gtk4::MenuButton::new();
        file_button.set_label("File");
        file_button.set_menu_model(Some(&menu));
        file_button.add_css_class("sd-editor-menu");
        let save_button = gtk4::Button::with_label("Save");
        save_button.set_action_name(Some("ed.save"));
        save_button.set_tooltip_text(Some("Save (Ctrl+S)"));
        let find_button = gtk4::Button::with_label("Find");
        find_button.set_action_name(Some("ed.find"));
        find_button.set_tooltip_text(Some("Find (Ctrl+F)"));
        let toolbar = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        toolbar.add_css_class("sd-editor-toolbar");
        toolbar.append(&file_button);
        toolbar.append(&save_button);
        toolbar.append(&find_button);
        body.append(&toolbar);

        let notebook = gtk4::Notebook::new();
        notebook.set_scrollable(true);
        notebook.set_show_border(false);
        notebook.set_vexpand(true);
        let empty = gtk4::Label::new(Some("Open a file from the tree, or start a new one (Ctrl+N)."));
        empty.add_css_class("sd-editor-empty");
        empty.set_wrap(true);
        let stack = gtk4::Stack::new();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&notebook, Some("files"));
        stack.set_visible_child_name("empty");
        stack.set_vexpand(true);

        let prompt = Prompt::new();
        let find = find::Find::new();
        let status = gtk4::Label::new(None);
        status.set_xalign(0.0);
        status.set_hexpand(true);
        status.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        let position = gtk4::Label::new(None);
        let status_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        status_row.add_css_class("sd-editor-status");
        status_row.append(&status);
        status_row.append(&position);
        let right = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        right.append(&prompt.widget);
        right.append(&stack);
        right.append(&find.widget);
        right.append(&status_row);

        // The tree needs the editor; it is made with weak links below.
        let weak_editor: Rc<RefCell<std::rc::Weak<Editor>>> = Rc::new(RefCell::new(std::rc::Weak::new()));
        let tree = tree::Tree::new(
            root,
            Rc::new({
                let weak = Rc::clone(&weak_editor);
                move |path| {
                    if let Some(editor) = weak.borrow().upgrade() {
                        editor.open_file(&path);
                    }
                }
            }),
            Rc::new({
                let weak = Rc::clone(&weak_editor);
                move |folder, path, widget, x, y| {
                    if let Some(editor) = weak.borrow().upgrade() {
                        editor.tree_menu(folder, path, widget, x, y);
                    }
                }
            }),
        );
        tree.set_show_hidden(show_hidden);
        let paned = gtk4::Paned::new(gtk4::Orientation::Horizontal);
        paned.set_start_child(Some(&tree.widget));
        paned.set_end_child(Some(&right));
        paned.set_position(220);
        paned.set_resize_start_child(false);
        paned.set_shrink_start_child(false);
        paned.set_vexpand(true);
        body.append(&paned);

        let on_close: Rc<dyn Fn()> = Rc::new({
            let weak = Rc::clone(&weak_editor);
            move || {
                if let Some(editor) = weak.borrow().upgrade() {
                    editor.request_close();
                }
            }
        });
        let saved: Rc<dyn Fn(Geometry)> = Rc::new(|_| persist());
        let title = format!("SD Editor · {}", pretty(root));
        let card = PluginCard::with_body(host, id, "✎", &title, &body, geometry, on_close, saved);
        card.container.add_css_class("sd-editor-card");
        if has_icon() {
            card.set_badge_icon(ICON);
        }

        let editor = Rc::new(Self {
            id: id.to_owned(),
            root: root.to_path_buf(),
            card,
            tree,
            stack,
            notebook,
            docs: RefCell::new(Vec::new()),
            previews: RefCell::new(Vec::new()),
            prompt,
            find,
            status,
            position,
            untitled: Cell::new(0),
            cursor_handler: RefCell::new(None),
        });
        *weak_editor.borrow_mut() = Rc::downgrade(&editor);
        editor.install_actions(&body, &actions);
        editor.notebook.connect_switch_page({
            let weak = Rc::downgrade(&editor);
            move |_, page, _| {
                let Some(editor) = weak.upgrade() else { return };
                let doc = editor.docs.borrow().iter().find(|d| d.page.upcast_ref::<gtk4::Widget>() == page).cloned();
                let preview = editor.previews.borrow().iter().find(|p| p.page.upcast_ref::<gtk4::Widget>() == page).cloned();
                if let Some(doc) = doc {
                    editor.follow(&doc);
                } else if let Some(preview) = preview {
                    editor.show_info(&preview);
                }
                // Not while a tab is being added or removed (persisted then).
                glib::idle_add_local_once(persist);
            }
        });
        EDITORS.with(|cell| cell.borrow_mut().push(Rc::clone(&editor)));
        editor
    }

    fn saved(&self) -> SavedEditor {
        let g = self.card.geometry();
        let files = self.tab_paths();
        SavedEditor {
            id: self.id.clone(),
            root: self.root.clone(),
            x: g.x, y: g.y, width: g.width, height: g.height,
            files,
            active: self.notebook.current_page().unwrap_or(0) as usize,
            show_hidden: self.tree.shows_hidden(),
        }
    }

    fn docs_in_tab_order(&self) -> Vec<Rc<Doc>> {
        let docs = self.docs.borrow();
        (0..self.notebook.n_pages())
            .filter_map(|i| self.notebook.nth_page(Some(i)))
            .filter_map(|page| docs.iter().find(|d| d.page.upcast_ref::<gtk4::Widget>() == &page).cloned())
            .collect()
    }

    /// Every open file's path, docs and previews, in tab order.
    fn tab_paths(&self) -> Vec<PathBuf> {
        let (docs, previews) = (self.docs.borrow(), self.previews.borrow());
        (0..self.notebook.n_pages())
            .filter_map(|i| self.notebook.nth_page(Some(i)))
            .filter_map(|page| {
                docs.iter().find(|d| d.page.upcast_ref::<gtk4::Widget>() == &page).and_then(|d| d.path.borrow().clone())
                    .or_else(|| previews.iter().find(|p| p.page.upcast_ref::<gtk4::Widget>() == &page).map(|p| p.path.borrow().clone()))
            })
            .collect()
    }

    fn current_preview(&self) -> Option<Rc<media::Preview>> {
        let page = self.notebook.nth_page(self.notebook.current_page())?;
        self.previews.borrow().iter().find(|p| p.page.upcast_ref::<gtk4::Widget>() == &page).cloned()
    }

    fn install_actions(self: &Rc<Self>, body: &gtk4::Box, group: &gio::SimpleActionGroup) {
        let add = |name: &str, run: fn(&Rc<Self>)| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(editor) = weak.upgrade() {
                    run(&editor);
                }
            });
            group.add_action(&action);
        };
        add("new", |e| e.new_file());
        add("open", |e| e.ask_open());
        add("save", |e| {
            if let Some(doc) = e.current() {
                e.save(&doc, |_| {});
            }
        });
        add("save-as", |e| {
            if let Some(doc) = e.current() {
                e.save_as(&doc, |_| {});
            }
        });
        add("close-file", |e| {
            if let Some(doc) = e.current() {
                e.close_doc(&doc);
            } else if let Some(preview) = e.current_preview() {
                e.remove_preview(&preview);
            }
        });
        add("close", |e| e.request_close());
        add("find", |e| e.show_find(false));
        add("replace", |e| e.show_find(true));
        add("goto", |e| e.ask_line());
        let hidden = gio::SimpleAction::new_stateful("hidden", None, &self.tree.shows_hidden().to_variant());
        hidden.connect_activate({
            let weak = Rc::downgrade(self);
            move |action, _| {
                let Some(editor) = weak.upgrade() else { return };
                let show = !editor.tree.shows_hidden();
                editor.tree.set_show_hidden(show);
                action.set_state(&show.to_variant());
                persist();
            }
        });
        group.add_action(&hidden);

        let shortcuts = gtk4::ShortcutController::new();
        shortcuts.set_propagation_phase(gtk4::PropagationPhase::Capture);
        for (keys, action) in [
            ("<Control>n", "ed.new"), ("<Control>o", "ed.open"), ("<Control>s", "ed.save"),
            ("<Control><Shift>s", "ed.save-as"), ("<Control>w", "ed.close-file"), ("<Control>f", "ed.find"),
            ("<Control>h", "ed.replace"), ("<Control>l", "ed.goto"),
        ] {
            shortcuts.add_shortcut(gtk4::Shortcut::new(gtk4::ShortcutTrigger::parse_string(keys), Some(gtk4::NamedAction::new(action))));
        }
        body.add_controller(shortcuts);

        // Esc closes the innermost question or bar; SUPER DESKTOP stays.
        let keys = gtk4::EventControllerKey::new();
        keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let weak = Rc::downgrade(self);
            move |_, key, _, _| {
                let Some(editor) = weak.upgrade() else { return glib::Propagation::Proceed };
                if key != gtk4::gdk::Key::Escape {
                    return glib::Propagation::Proceed;
                }
                if editor.prompt.is_open() {
                    editor.prompt.answer(Answer::No);
                } else if editor.find.is_open() {
                    editor.find.hide();
                    editor.focus();
                }
                glib::Propagation::Stop
            }
        });
        body.add_controller(keys);
    }

    fn current(&self) -> Option<Rc<Doc>> {
        let page = self.notebook.nth_page(self.notebook.current_page())?;
        self.docs.borrow().iter().find(|d| d.page.upcast_ref::<gtk4::Widget>() == &page).cloned()
    }

    /// The open file's view takes the keyboard.
    fn focus(&self) {
        if let Some(doc) = self.current() {
            doc.view.grab_focus();
        }
    }

    fn say(&self, text: &str) {
        self.status.set_text(text);
    }

    /// A path typed in the card: absolute, `~/…`, or in the editor's folder.
    fn resolve(&self, typed: &str) -> PathBuf {
        let typed = typed.trim();
        if let Some(rest) = typed.strip_prefix("~/") {
            return PathBuf::from(crate::state::home_dir_string()).join(rest);
        }
        let path = Path::new(typed);
        if path.is_absolute() { path.to_path_buf() } else { self.root.join(path) }
    }

    /// The status line shows a preview's size, pixels or length.
    fn show_info(&self, preview: &Rc<media::Preview>) {
        if let Some((buffer, handler)) = self.cursor_handler.take() {
            buffer.disconnect(handler);
        }
        self.position.set_text(&preview.info.borrow());
    }

    /// The status line and find bar follow the open file.
    fn follow(&self, doc: &Rc<Doc>) {
        if let Some((buffer, handler)) = self.cursor_handler.take() {
            buffer.disconnect(handler);
        }
        let position = self.position.clone();
        let show = move |buffer: &sourceview5::Buffer| {
            let at = buffer.iter_at_mark(&buffer.get_insert());
            position.set_text(&format!("Ln {}, Col {}", at.line() + 1, at.line_offset() + 1));
        };
        show(&doc.buffer);
        let handler = doc.buffer.connect_cursor_position_notify(move |buffer| show(buffer));
        *self.cursor_handler.borrow_mut() = Some((doc.buffer.clone(), handler));
        if self.find.is_open() {
            self.find.attach(&doc.buffer, &doc.view);
        }
    }

    fn add_doc(self: &Rc<Self>, doc: Rc<Doc>) {
        doc.close.connect_clicked({
            let (weak, doc) = (Rc::downgrade(self), Rc::downgrade(&doc));
            move |_| {
                if let (Some(editor), Some(doc)) = (weak.upgrade(), doc.upgrade()) {
                    editor.close_doc(&doc);
                }
            }
        });
        *doc.on_disk_change.borrow_mut() = Some(Rc::new({
            let weak = Rc::downgrade(self);
            move |doc: &Rc<Doc>| {
                if let Some(editor) = weak.upgrade() {
                    editor.ask_reload(doc);
                }
            }
        }));
        self.docs.borrow_mut().push(Rc::clone(&doc));
        let index = self.notebook.append_page(&doc.page, Some(&doc.tab));
        self.notebook.set_tab_reorderable(&doc.page, true);
        self.stack.set_visible_child_name("files");
        self.notebook.set_current_page(Some(index));
        self.follow(&doc);
        doc.view.grab_focus();
        persist();
    }

    fn new_file(self: &Rc<Self>) {
        self.untitled.set(self.untitled.get() + 1);
        self.add_doc(Doc::new(self.untitled.get(), scheme::current().as_ref()));
    }

    /// Opens `path` in a tab (an image, a sound or a video in a preview),
    /// or shows its tab when it is open already.
    pub fn open_file(self: &Rc<Self>, path: &Path) {
        let open = self.docs.borrow().iter().find(|d| d.path.borrow().as_deref() == Some(path)).cloned();
        if let Some(doc) = open {
            if let Some(index) = self.notebook.page_num(&doc.page) {
                self.notebook.set_current_page(Some(index));
            }
            doc.view.grab_focus();
            return;
        }
        let shown = self.previews.borrow().iter().find(|p| p.path.borrow().as_path() == path).cloned();
        if let Some(preview) = shown {
            if let Some(index) = self.notebook.page_num(&preview.page) {
                self.notebook.set_current_page(Some(index));
            }
            return;
        }
        let kind = media::kind(path);
        if kind != media::Kind::Text {
            if !path.is_file() {
                return self.say(&format!("{} is not a file", pretty(path)));
            }
            return self.add_preview(media::Preview::new(path, kind));
        }
        let doc = Doc::new(0, scheme::current().as_ref());
        match doc.load(path) {
            Ok(()) => {
                self.add_doc(doc);
                self.say(&format!("Opened {}", pretty(path)));
            }
            Err(e) => self.say(&e),
        }
    }

    fn add_preview(self: &Rc<Self>, preview: Rc<media::Preview>) {
        preview.close.connect_clicked({
            let (weak, target) = (Rc::downgrade(self), Rc::downgrade(&preview));
            move |_| {
                if let (Some(editor), Some(preview)) = (weak.upgrade(), target.upgrade()) {
                    editor.remove_preview(&preview);
                }
            }
        });
        *preview.on_info.borrow_mut() = Some(Rc::new({
            let (weak, target) = (Rc::downgrade(self), Rc::downgrade(&preview));
            move || {
                let (Some(editor), Some(preview)) = (weak.upgrade(), target.upgrade()) else { return };
                if editor.current_preview().is_some_and(|p| Rc::ptr_eq(&p, &preview)) {
                    editor.show_info(&preview);
                }
            }
        }));
        self.previews.borrow_mut().push(Rc::clone(&preview));
        let index = self.notebook.append_page(&preview.page, Some(&preview.tab));
        self.notebook.set_tab_reorderable(&preview.page, true);
        self.stack.set_visible_child_name("files");
        self.notebook.set_current_page(Some(index));
        self.show_info(&preview);
        self.say(&format!("Opened {}", pretty(&preview.path.borrow())));
        persist();
    }

    fn remove_preview(&self, preview: &Rc<media::Preview>) {
        preview.stop();
        if let Some(index) = self.notebook.page_num(&preview.page) {
            self.notebook.remove_page(Some(index));
        }
        self.previews.borrow_mut().retain(|p| !Rc::ptr_eq(p, preview));
        self.after_tab_closed();
    }

    fn ask_open(self: &Rc<Self>) {
        let start = self.current().and_then(|d| d.path.borrow().as_ref().and_then(|p| p.parent()).map(Path::to_path_buf)).unwrap_or_else(|| self.root.clone());
        let text = format!("{}/", start.strip_prefix(&self.root).map_or_else(|_| start.display().to_string(), |p| p.display().to_string()));
        let text = if text == "/" { String::new() } else { text };
        let weak = Rc::downgrade(self);
        self.prompt.ask(&format!("Open a file (in {}, or a full path)", pretty(&self.root)), Some(&text), "Open", None, move |answer| {
            let (Some(editor), Answer::Yes(typed)) = (weak.upgrade(), answer) else { return };
            if !typed.is_empty() {
                editor.open_file(&editor.resolve(&typed));
            }
        });
    }

    /// Saves `doc`, asking for a name when it has none; `then` learns
    /// whether it was saved.
    fn save(self: &Rc<Self>, doc: &Rc<Doc>, then: impl FnOnce(bool) + 'static) {
        if doc.path.borrow().is_none() {
            return self.save_as(doc, then);
        }
        match doc.save() {
            Ok(()) => {
                self.say(&format!("Saved {}", doc.title()));
                then(true);
            }
            Err(e) => {
                self.say(&e);
                then(false);
            }
        }
    }

    fn save_as(self: &Rc<Self>, doc: &Rc<Doc>, then: impl FnOnce(bool) + 'static) {
        let current = doc.path.borrow().clone().unwrap_or_else(|| self.root.join(format!("untitled-{}.txt", doc_number(doc))));
        let shown = current.strip_prefix(&self.root).map_or_else(|_| current.display().to_string(), |p| p.display().to_string());
        let (weak, target) = (Rc::downgrade(self), Rc::downgrade(doc));
        let then = RefCell::new(Some(then));
        self.prompt.ask(&format!("Save {} as (in {}, or a full path)", doc.title(), pretty(&self.root)), Some(&shown), "Save", None, move |answer| {
            let (Some(editor), Some(doc), Some(then)) = (weak.upgrade(), target.upgrade(), then.take()) else { return };
            let Answer::Yes(typed) = answer else { return then(false) };
            if typed.is_empty() {
                return then(false);
            }
            let path = editor.resolve(&typed);
            let write = {
                let (editor, doc) = (Rc::clone(&editor), Rc::clone(&doc));
                move |path: PathBuf, then: Box<dyn FnOnce(bool)>| match doc.save_as(&path) {
                    Ok(()) => {
                        editor.say(&format!("Saved {}", pretty(&path)));
                        persist();
                        then(true);
                    }
                    Err(e) => {
                        editor.say(&e);
                        then(false);
                    }
                }
            };
            if path.exists() && doc.path.borrow().as_deref() != Some(path.as_path()) {
                let shown = pretty(&path);
                let then = RefCell::new(Some(Box::new(then) as Box<dyn FnOnce(bool)>));
                editor.prompt.ask(&format!("{shown} exists. Replace it?"), None, "Replace", None, move |answer| {
                    let Some(then) = then.take() else { return };
                    match answer {
                        Answer::Yes(_) => write(path.clone(), then),
                        _ => then(false),
                    }
                });
            } else {
                write(path, Box::new(then));
            }
        });
    }

    /// Closes a tab, asking first when it has unsaved changes.
    fn close_doc(self: &Rc<Self>, doc: &Rc<Doc>) {
        if !doc.is_modified() {
            return self.remove_doc(doc);
        }
        if let Some(index) = self.notebook.page_num(&doc.page) {
            self.notebook.set_current_page(Some(index));
        }
        let (weak, target) = (Rc::downgrade(self), Rc::downgrade(doc));
        self.prompt.ask(&format!("Save changes to {} before closing it?", doc.title()), None, "Save", Some("Don't save"), move |answer| {
            let (Some(editor), Some(doc)) = (weak.upgrade(), target.upgrade()) else { return };
            match answer {
                Answer::Yes(_) => {
                    let (closing, closed) = (Rc::clone(&editor), Rc::clone(&doc));
                    editor.save(&doc, move |saved| {
                        if saved {
                            closing.remove_doc(&closed);
                        }
                    });
                }
                Answer::Other => editor.remove_doc(&doc),
                Answer::No => {}
            }
        });
    }

    fn remove_doc(&self, doc: &Rc<Doc>) {
        if let Some(index) = self.notebook.page_num(&doc.page) {
            self.notebook.remove_page(Some(index));
        }
        doc.forget();
        self.docs.borrow_mut().retain(|d| !Rc::ptr_eq(d, doc));
        if self.docs.borrow().is_empty() {
            self.find.hide();
        }
        self.after_tab_closed();
    }

    fn after_tab_closed(&self) {
        if self.notebook.n_pages() == 0 {
            self.stack.set_visible_child_name("empty");
            self.position.set_text("");
            if let Some((buffer, handler)) = self.cursor_handler.take() {
                buffer.disconnect(handler);
            }
        }
        persist();
    }

    /// ✕ or File → Close editor: asks first when files have unsaved changes.
    fn request_close(self: &Rc<Self>) {
        let unsaved: Vec<Rc<Doc>> = self.docs_in_tab_order().into_iter().filter(|d| d.is_modified()).collect();
        if unsaved.is_empty() {
            return self.close_now();
        }
        let names: Vec<String> = unsaved.iter().map(|d| d.title()).collect();
        let weak = Rc::downgrade(self);
        self.prompt.ask(&format!("Unsaved changes in {}. Save them before closing the editor?", names.join(", ")), None, "Save all", Some("Close without saving"), move |answer| {
            let Some(editor) = weak.upgrade() else { return };
            match answer {
                Answer::Yes(_) => editor.save_all_then_close(),
                Answer::Other => editor.close_now(),
                Answer::No => {}
            }
        });
    }

    /// Saves every changed file that has a name; one without a name is
    /// shown so the user can save it, and the editor stays open.
    fn save_all_then_close(self: &Rc<Self>) {
        let unsaved: Vec<Rc<Doc>> = self.docs_in_tab_order().into_iter().filter(|d| d.is_modified()).collect();
        for doc in &unsaved {
            if doc.path.borrow().is_some() {
                if let Err(e) = doc.save() {
                    return self.say(&e);
                }
            }
        }
        if let Some(untitled) = unsaved.iter().find(|d| d.is_modified()) {
            if let Some(index) = self.notebook.page_num(&untitled.page) {
                self.notebook.set_current_page(Some(index));
            }
            let weak = Rc::downgrade(self);
            return self.save_as(untitled, move |saved| {
                if let (true, Some(editor)) = (saved, weak.upgrade()) {
                    editor.save_all_then_close();
                }
            });
        }
        self.close_now();
    }

    fn close_now(&self) {
        for doc in self.docs.borrow().iter() {
            doc.forget();
        }
        for preview in self.previews.borrow().iter() {
            preview.stop();
        }
        if let Some(host) = host() {
            self.card.remove(&host);
        }
        EDITORS.with(|cell| cell.borrow_mut().retain(|e| e.id != self.id));
        persist();
    }

    fn ask_reload(self: &Rc<Self>, doc: &Rc<Doc>) {
        if let Some(index) = self.notebook.page_num(&doc.page) {
            self.notebook.set_current_page(Some(index));
        }
        let target = Rc::downgrade(doc);
        let weak = Rc::downgrade(self);
        self.prompt.ask(&format!("{} changed on disk, and you have unsaved changes here.", doc.title()), None, "Reload from disk", Some("Keep mine"), move |answer| {
            let (Some(editor), Some(doc)) = (weak.upgrade(), target.upgrade()) else { return };
            match answer {
                Answer::Yes(_) => {
                    if let Err(e) = doc.reload() {
                        editor.say(&e);
                    }
                }
                // Kept: saving now overwrites the other program's change.
                _ => doc.stale.set(false),
            }
        });
    }

    fn show_find(&self, replace: bool) {
        if let Some(doc) = self.current() {
            self.find.show(&doc.buffer, &doc.view, replace);
        }
    }

    fn ask_line(self: &Rc<Self>) {
        let Some(doc) = self.current() else { return };
        let weak = Rc::downgrade(&doc);
        self.prompt.ask(&format!("Go to line (1 to {})", doc.buffer.line_count()), Some(""), "Go", None, move |answer| {
            let (Some(doc), Answer::Yes(typed)) = (weak.upgrade(), answer) else { return };
            if let Ok(line) = typed.parse::<i32>() {
                doc.go_to_line(line);
                doc.view.grab_focus();
            }
        });
    }

    /// The tree's right-click menu: new file or folder in `folder`; rename or
    /// trash `path`.
    fn tree_menu(self: &Rc<Self>, folder: PathBuf, path: Option<PathBuf>, widget: &gtk4::Widget, x: f64, y: f64) {
        let popover = gtk4::Popover::new();
        popover.add_css_class("ws-pop");
        let column = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
        column.add_css_class("sd-editor-tree-menu");
        let item = |label: &str, run: Box<dyn Fn()>| {
            let button = gtk4::Button::with_label(label);
            button.add_css_class("flat");
            if let Some(child) = button.child().and_downcast::<gtk4::Label>() {
                child.set_xalign(0.0);
            }
            let weak = popover.downgrade();
            button.connect_clicked(move |_| {
                if let Some(popover) = weak.upgrade() {
                    popover.popdown();
                }
                run();
            });
            column.append(&button);
        };
        let weak = Rc::downgrade(self);
        let in_folder = folder.clone();
        item("New file…", Box::new({
            let weak = weak.clone();
            move || if let Some(e) = weak.upgrade() { e.ask_new(&in_folder, false) }
        }));
        item("New folder…", Box::new({
            let (weak, folder) = (weak.clone(), folder.clone());
            move || if let Some(e) = weak.upgrade() { e.ask_new(&folder, true) }
        }));
        if let Some(path) = path {
            let target = path.clone();
            item("Rename…", Box::new({
                let weak = weak.clone();
                move || if let Some(e) = weak.upgrade() { e.ask_rename(&target) }
            }));
            item("Move to trash", Box::new({
                let weak = weak.clone();
                move || if let Some(e) = weak.upgrade() { e.ask_trash(&path) }
            }));
        }
        popover.set_child(Some(&column));
        popover.set_parent(widget);
        popover.set_pointing_to(Some(&gtk4::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.connect_closed(|popover| {
            let popover = popover.clone();
            glib::idle_add_local_once(move || popover.unparent());
        });
        popover.popup();
    }

    fn ask_new(self: &Rc<Self>, folder: &Path, directory: bool) {
        let weak = Rc::downgrade(self);
        let folder = folder.to_path_buf();
        let what = if directory { "folder" } else { "file" };
        self.prompt.ask(&format!("New {what} in {}", pretty(&folder)), Some(""), "Create", None, move |answer| {
            let (Some(editor), Answer::Yes(name)) = (weak.upgrade(), answer) else { return };
            if name.is_empty() || name.contains('/') {
                return editor.say("Give a name without /.");
            }
            let path = folder.join(&name);
            let made = if directory {
                std::fs::create_dir(&path)
            } else {
                std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map(|_| ())
            };
            match made {
                Ok(()) if !directory => editor.open_file(&path),
                Ok(()) => editor.say(&format!("Made {}", pretty(&path))),
                Err(e) => editor.say(&format!("Cannot make {}: {e}", pretty(&path))),
            }
        });
    }

    fn ask_rename(self: &Rc<Self>, path: &Path) {
        let weak = Rc::downgrade(self);
        let from = path.to_path_buf();
        let name = from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let old_name = name.clone();
        self.prompt.ask(&format!("Rename {name} to"), Some(&name), "Rename", None, move |answer| {
            let (Some(editor), Answer::Yes(new_name)) = (weak.upgrade(), answer) else { return };
            if new_name.is_empty() || new_name.contains('/') || new_name == old_name {
                return;
            }
            let to = from.with_file_name(&new_name);
            if to.exists() {
                return editor.say(&format!("{} exists already", pretty(&to)));
            }
            if let Err(e) = std::fs::rename(&from, &to) {
                return editor.say(&format!("Cannot rename: {e}"));
            }
            // Open files follow, also those inside a renamed folder.
            for doc in editor.docs.borrow().iter() {
                let moved = doc.path.borrow().as_ref().and_then(|p| p.strip_prefix(&from).ok().map(|rest| to.join(rest)));
                if let Some(moved) = moved {
                    doc.moved_to(&moved);
                }
            }
            for preview in editor.previews.borrow().iter() {
                let moved = preview.path.borrow().strip_prefix(&from).ok().map(|rest| to.join(rest));
                if let Some(moved) = moved {
                    preview.moved_to(&moved);
                }
            }
            editor.say(&format!("Renamed to {new_name}"));
            persist();
        });
    }

    fn ask_trash(self: &Rc<Self>, path: &Path) {
        let weak = Rc::downgrade(self);
        let target = path.to_path_buf();
        self.prompt.ask(&format!("Move {} to the trash?", pretty(path)), None, "Move to trash", None, move |answer| {
            let (Some(editor), Answer::Yes(_)) = (weak.upgrade(), answer) else { return };
            if let Err(e) = gio::File::for_path(&target).trash(None::<&gio::Cancellable>) {
                return editor.say(&format!("Cannot move to the trash: {}", e.message()));
            }
            // Its tabs close; their text is in the trash too.
            let gone: Vec<Rc<Doc>> = editor.docs.borrow().iter().filter(|d| d.path.borrow().as_ref().is_some_and(|p| p.starts_with(&target))).cloned().collect();
            for doc in gone {
                editor.remove_doc(&doc);
            }
            let gone: Vec<Rc<media::Preview>> = editor.previews.borrow().iter().filter(|p| p.path.borrow().starts_with(&target)).cloned().collect();
            for preview in gone {
                editor.remove_preview(&preview);
            }
            editor.say(&format!("Moved {} to the trash", pretty(&target)));
        });
    }
}

fn doc_number(doc: &Doc) -> String {
    doc.title().rsplit(' ').next().unwrap_or("1").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_previews_images_sound_and_video() {
        crate::gtk_test::run_in_child_process("editor::tests::editor_previews_inner");
    }

    /// Images, sound and video open in preview tabs: the picture with its
    /// size, the player that does not start by itself, a format that cannot
    /// play saying what adds it; previews are kept, closed and renamed like
    /// files.
    #[test]
    fn editor_previews_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let root = std::env::temp_dir().join(format!("sd-editor-media-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for name in ["HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"] {
            std::env::set_var(name, &root);
        }
        let project = root.join("media");
        std::fs::create_dir_all(&project).unwrap();
        let project = project.canonicalize().unwrap();
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let pixbuf = gtk4::gdk_pixbuf::Pixbuf::new(gtk4::gdk_pixbuf::Colorspace::Rgb, false, 8, 40, 30).unwrap();
        pixbuf.fill(0x3366ccff);
        pixbuf.savev(project.join("pic.png"), "png", &[]).unwrap();
        std::fs::write(project.join("tune.ogg"), b"OggS not really").unwrap();
        std::fs::write(project.join("clip.webm"), b"not really a video").unwrap();
        let canvas = gtk4::Fixed::new();
        let window = gtk4::Window::new();
        window.set_default_size(1100, 760);
        window.set_child(Some(&canvas));
        window.present();
        set_host(Rc::new(CanvasHost {
            canvas: canvas.clone(),
            raise: Rc::new(|_| {}),
            drag: Rc::new(|_, _, _| {}),
            drag_end: Rc::new(|_, x, y| (x, y)),
            size: Rc::new(|| (1100, 760)),
            top: Rc::new(|| 46),
        }));
        open(&project, Some(Path::new("pic.png"))).unwrap();
        let editor = editors().pop().unwrap();
        let find = |page: &gtk4::Box| crate::gtk_test::descendants(page.upcast_ref());

        // The image: decoded off GTK's thread, its size on the status line.
        let pic = editor.current_preview().expect("a preview tab");
        assert_eq!(pic.kind, media::Kind::Image);
        assert!(editor.current().is_none(), "a preview is not a text file");
        pump_until("the picture", || find(&pic.page).iter().any(|w| w.is::<gtk4::Picture>()));
        pump_until("its size", || editor.position.text().starts_with("40 × 30"));

        // Sound and video: GTK's player, not playing until Play.
        editor.open_file(&project.join("tune.ogg"));
        let tune = editor.current_preview().unwrap();
        assert_eq!(tune.kind, media::Kind::Audio);
        assert!(find(&tune.page).iter().any(|w| w.is::<gtk4::MediaControls>()));
        editor.open_file(&project.join("clip.webm"));
        let clip = editor.current_preview().unwrap();
        assert_eq!(clip.kind, media::Kind::Video);
        let video = find(&clip.page).into_iter().find_map(|w| w.downcast::<gtk4::Video>().ok()).expect("a video");
        assert!(!video.is_autoplay());
        for preview in [&tune, &clip] {
            assert!(!preview.media().unwrap().is_playing());
        }
        // A file this PC cannot play says which packages add more formats.
        clip.media().unwrap().set_error(glib::Error::new(gio::IOErrorEnum::NotSupported, "no decoder for video/webm"));
        pump_until("the message", || find(&clip.page).iter().filter_map(|w| w.downcast_ref::<gtk4::Label>().cloned())
            .any(|l| l.is_visible() && l.text().contains("gst-plugins-good")));
        // Pictures for a visual check: SD_EDITOR_SHOTS=<dir> cargo test editor_previews.
        if let Some(dir) = std::env::var_os("SD_EDITOR_SHOTS") {
            for (preview, name) in [(&pic, "image"), (&tune, "sound"), (&clip, "video")] {
                editor.notebook.set_current_page(editor.notebook.page_num(&preview.page));
                crate::gtk_test::pump(300);
                crate::gtk_test::save_png(&editor.card.container, &PathBuf::from(&dir).join(format!("preview-{name}.png")));
            }
        }
        // Opening it again shows its tab.
        editor.open_file(&project.join("pic.png"));
        assert_eq!(editor.previews.borrow().len(), 3);
        assert!(editor.current_preview().is_some_and(|p| Rc::ptr_eq(&p, &pic)));

        // Kept with the files; closed and renamed like them.
        assert_eq!(editor.tab_paths(), [project.join("pic.png"), project.join("tune.ogg"), project.join("clip.webm")]);
        let saved: Saved = serde_json::from_str(&std::fs::read_to_string(saved_path()).unwrap()).unwrap();
        assert_eq!(saved.editors[0].files.len(), 3);
        editor.notebook.set_current_page(editor.notebook.page_num(&tune.page));
        gtk4::prelude::WidgetExt::activate_action(&editor.notebook, "ed.close-file", None).unwrap();
        assert_eq!(editor.previews.borrow().len(), 2);
        assert!(tune.media().unwrap().file().is_none(), "a closed preview lets go of its file");
        editor.ask_rename(&project.join("clip.webm"));
        editor.prompt.entry.set_text("movie.webm");
        editor.prompt.yes.emit_clicked();
        assert_eq!(*clip.path.borrow(), project.join("movie.webm"));
        assert_eq!(clip.media().unwrap().file().and_then(|f| f.path()), Some(project.join("movie.webm")));
        editor.close_now();
        window.close();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn editor_opens_edits_saves_and_comes_back() {
        crate::gtk_test::run_in_child_process("editor::tests::editor_flow_inner");
    }

    fn pump_until(what: &str, done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done() && std::time::Instant::now() < deadline {
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(done(), "timed out: {what}");
    }

    /// The whole life of an editor card: its tree, opening, editing, saving,
    /// Save as, questions asked in the card, changes made on disk, closing
    /// with unsaved changes, and coming back after a restart.
    #[test]
    fn editor_flow_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let root = std::env::temp_dir().join(format!("sd-editor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for name in ["HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"] {
            std::env::set_var(name, &root);
        }
        let project = root.join("project");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(project.join("a.txt"), "hello\n").unwrap();
        std::fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(project.join(".hidden"), "secret\n").unwrap();
        let project = project.canonicalize().unwrap();
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        gtk4::IconTheme::for_display(&gtk4::gdk::Display::default().unwrap())
            .add_search_path(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/icons"));
        let canvas = gtk4::Fixed::new();
        let window = gtk4::Window::new();
        window.set_default_size(1100, 760);
        window.set_child(Some(&canvas));
        window.present();
        set_host(Rc::new(CanvasHost {
            canvas: canvas.clone(),
            raise: Rc::new(|_| {}),
            drag: Rc::new(|_, _, _| {}),
            drag_end: Rc::new(|_, x, y| (x, y)),
            size: Rc::new(|| (1100, 760)),
            top: Rc::new(|| 46),
        }));

        open(&project, None).unwrap();
        let editor = editors().pop().expect("an editor");
        assert_eq!(editor.card.geometry().width, DEFAULT_SIZE.0);
        // The tree: folders first, no hidden files until asked.
        pump_until("the tree", || editor.tree.shown().len() == 2);
        assert_eq!(editor.tree.shown(), [project.join("src"), project.join("a.txt")]);
        editor.tree.set_show_hidden(true);
        pump_until("hidden files", || editor.tree.shown().len() == 3);
        editor.tree.set_show_hidden(false);
        pump_until("hidden files gone", || editor.tree.shown().len() == 2);
        editor.tree.expand(&project.join("src"));
        pump_until("src expanded", || editor.tree.shown().contains(&project.join("src/main.rs")));
        // Opening the same folder again brings this editor back.
        open(&project, Some(Path::new("a.txt"))).unwrap();
        assert_eq!(editors().len(), 1);
        let a = editor.current().unwrap();
        assert_eq!(a.text(), "hello\n");
        assert!(!a.is_modified());
        editor.open_file(&project.join("src/main.rs"));
        let main = editor.current().unwrap();
        assert_eq!(main.buffer.language().map(|l| l.id().to_string()).as_deref(), Some("rust"));
        assert_eq!(editor.docs.borrow().len(), 2);
        editor.open_file(&project.join("a.txt"));
        assert_eq!(editor.docs.borrow().len(), 2, "an open file is not opened twice");

        // The toolbar's Save runs the editor's action.
        let save_button = crate::gtk_test::descendants(editor.card.container.upcast_ref()).into_iter()
            .filter_map(|w| w.downcast::<gtk4::Button>().ok())
            .find(|b| b.label().as_deref() == Some("Save")).unwrap();
        assert!(save_button.is_sensitive(), "the toolbar finds the editor's actions");
        // Edit and save.
        a.buffer.insert(&mut a.buffer.end_iter(), "world\n");
        assert!(a.is_modified());
        assert!(a.tab.first_child().and_downcast::<gtk4::Label>().unwrap().text().starts_with("● "));
        editor.save(&a, |saved| assert!(saved));
        assert_eq!(std::fs::read_to_string(project.join("a.txt")).unwrap(), "hello\nworld\n");
        assert!(!a.is_modified());

        // A new file, saved as new.txt through the question in the card.
        editor.new_file();
        let fresh = editor.current().unwrap();
        assert_eq!(fresh.title(), "Untitled 1");
        fresh.buffer.set_text("fresh\n");
        editor.save(&fresh, |_| {});
        assert!(editor.prompt.is_open(), "an untitled file asks for a name");
        editor.prompt.entry.set_text("new.txt");
        editor.prompt.yes.emit_clicked();
        assert_eq!(std::fs::read_to_string(project.join("new.txt")).unwrap(), "fresh\n");
        assert_eq!(fresh.title(), "new.txt");
        // The tree shows it, though saving a.txt replaced that file by a
        // rename (GTK's own folder list stops following a folder then), and
        // a file another program makes; src stays expanded.
        pump_until("new.txt in the tree", || editor.tree.shown().contains(&project.join("new.txt")));
        std::fs::write(project.join("other.txt"), "x").unwrap();
        pump_until("other.txt in the tree", || editor.tree.shown().contains(&project.join("other.txt")));
        assert!(editor.tree.shown().contains(&project.join("src/main.rs")), "src stays expanded");
        std::fs::remove_file(project.join("other.txt")).unwrap();
        pump_until("other.txt gone", || !editor.tree.shown().contains(&project.join("other.txt")));
        assert_eq!(editor.tree.watched(), 2, "the folder and src are watched");
        // Save as over another file asks first; no keeps it.
        editor.save_as(&fresh, |_| {});
        editor.prompt.entry.set_text("a.txt");
        editor.prompt.yes.emit_clicked();
        assert!(editor.prompt.is_open() && editor.prompt.question.text().contains("exists"));
        editor.prompt.no.emit_clicked();
        assert_eq!(std::fs::read_to_string(project.join("a.txt")).unwrap(), "hello\nworld\n");

        // Closing a changed file asks; "Don't save" closes it unsaved.
        fresh.buffer.insert(&mut fresh.buffer.end_iter(), "more\n");
        editor.close_doc(&fresh);
        assert!(editor.prompt.is_open());
        editor.prompt.other.emit_clicked();
        assert_eq!(editor.docs.borrow().len(), 2);
        assert_eq!(std::fs::read_to_string(project.join("new.txt")).unwrap(), "fresh\n");

        // Another program changes a file: an unedited one reloads...
        std::fs::write(project.join("src/main.rs"), "fn main() { println!(\"hi\"); }\n").unwrap();
        pump_until("the reload", || main.text().contains("println"));
        assert!(!main.is_modified());
        // ...an edited one asks.
        a.buffer.insert(&mut a.buffer.end_iter(), "mine\n");
        std::fs::write(project.join("a.txt"), "theirs\n").unwrap();
        pump_until("the question", || editor.prompt.is_open());
        assert!(editor.prompt.question.text().contains("changed on disk"));
        editor.prompt.yes.emit_clicked();
        assert_eq!(a.text(), "theirs\n");

        // Find: every match counted in the open file.
        editor.notebook.set_current_page(editor.notebook.page_num(&a.page));
        editor.show_find(false);
        editor.find.search.set_text("their");
        pump_until("the match count", || editor.find.is_open() && a.buffer.selection_bounds().is_some());
        // A picture for a visual check: SD_EDITOR_SHOTS=<dir> cargo test editor_opens.
        if let Some(dir) = std::env::var_os("SD_EDITOR_SHOTS") {
            editor.open_file(&project.join("src/main.rs"));
            crate::gtk_test::pump(300);
            crate::gtk_test::save_png(&editor.card.container, &PathBuf::from(dir).join("editor.png"));
            editor.notebook.set_current_page(editor.notebook.page_num(&a.page));
        }

        // Which editors and files are open is kept.
        let saved: Saved = serde_json::from_str(&std::fs::read_to_string(saved_path()).unwrap()).unwrap();
        assert_eq!(saved.editors.len(), 1);
        assert_eq!(saved.editors[0].root, project);
        assert_eq!(saved.editors[0].files, [project.join("a.txt"), project.join("src/main.rs")]);

        // A plugin opens an editor with editor.open: another folder, at a
        // file in it; a folder that is not there is refused.
        let other = root.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("notes.md"), "# notes\n").unwrap();
        crate::plugin_ui::api::set_hooks(crate::plugin_ui::api::Hooks {
            hide: Rc::new(|| {}),
            harnesses: Rc::new(|| serde_json::json!([])),
            launch: Rc::new(|_, _| Ok(())),
            cards: Rc::new(|| serde_json::json!([])),
            open_card: Rc::new(|_, _| Err("no".into())),
            focus_card: Rc::new(|_| Ok(())),
            card_session: Rc::new(|_| None),
        });
        let spec = crate::plugin_ui::surface::SurfaceSpec {
            plugin_id: "sd-editor".into(), plugin_name: "SD Editor".into(), surface_id: "button".into(), instance: String::new(),
            qml: root.join("Bar.qml"), dir: root.clone(),
        };
        let call = |params: serde_json::Value| crate::plugin_ui::api::call(&spec, "editor.open", &params);
        call(serde_json::json!({"folder": other, "file": "notes.md"})).unwrap();
        pump_until("the second editor", || editors().len() == 2);
        let second = editors().into_iter().find(|e| e.root == other.canonicalize().unwrap()).unwrap();
        assert_eq!(second.current().unwrap().buffer.language().map(|l| l.id().to_string()).as_deref(), Some("markdown"));
        assert_eq!(call(serde_json::json!({"folder": root.join("nope")})).unwrap_err()["code"], "bad_params");
        second.close_now();

        // ✕ with unsaved changes asks; "Close without saving" closes.
        a.buffer.insert(&mut a.buffer.end_iter(), "unsaved\n");
        editor.request_close();
        assert!(editor.prompt.is_open());
        editor.prompt.other.emit_clicked();
        assert!(editors().is_empty());
        assert!(editor.card.container.parent().is_none(), "the card left the canvas");
        let saved: Saved = serde_json::from_str(&std::fs::read_to_string(saved_path()).unwrap()).unwrap();
        assert!(saved.editors.is_empty());

        // After a restart: the saved editor and its files come back.
        std::fs::write(saved_path(), serde_json::json!({"editors": [{"id": "editor-7", "root": project, "x": 40, "y": 90,
            "width": 700, "height": 500, "files": [project.join("a.txt"), project.join("gone.txt")], "active": 0, "show_hidden": true}]}).to_string()).unwrap();
        restore();
        let back = editors().pop().expect("the restored editor");
        assert_eq!((back.id.as_str(), back.card.geometry().width), ("editor-7", 700));
        assert_eq!(back.docs.borrow().len(), 1, "a file that is gone is skipped");
        assert!(back.tree.shows_hidden());
        back.close_now();
        window.close();
        let _ = std::fs::remove_dir_all(root);
    }
}
