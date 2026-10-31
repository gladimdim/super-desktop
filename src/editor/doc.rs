//! One file open in SD Editor: a GtkSourceView buffer and view, its tab, and
//! loading and saving. Saving replaces the file atomically through GIO,
//! which keeps its permissions; a change made on disk by another program is
//! noticed and offered for a reload.
use gtk4::prelude::*;
use gtk4::{gio, glib};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Told about a file another program changed under unsaved edits.
type DiskChange = Rc<dyn Fn(&Rc<Doc>)>;

/// Larger files are refused: the view would stall the whole overlay.
pub const MAX_BYTES: usize = 8 * 1024 * 1024;

pub struct Doc {
    pub path: RefCell<Option<PathBuf>>,
    pub buffer: sourceview5::Buffer,
    pub view: sourceview5::View,
    /// The notebook page.
    pub page: gtk4::ScrolledWindow,
    pub tab: gtk4::Box,
    pub close: gtk4::Button,
    label: gtk4::Label,
    /// "Untitled 3" until it is saved somewhere.
    untitled: u32,
    monitor: RefCell<Option<gio::FileMonitor>>,
    /// Told when another program changed the file while the buffer has
    /// edits of its own (an unedited buffer just reloads).
    pub on_disk_change: RefCell<Option<DiskChange>>,
    /// Told when its title or modified mark changes.
    pub on_title: RefCell<Option<Rc<dyn Fn()>>>,
    /// The file changed on disk and the user has not decided yet.
    pub stale: Cell<bool>,
}

/// Why a file could not be opened.
pub fn read(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if bytes.len() > MAX_BYTES {
        return Err(format!("{} is larger than 8 MiB", path.display()));
    }
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return Err(format!("{} is a binary file", path.display()));
    }
    String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8 text", path.display()))
}

impl Doc {
    pub fn new(untitled: u32, scheme: Option<&sourceview5::StyleScheme>) -> Rc<Self> {
        let buffer = sourceview5::Buffer::new(None);
        buffer.set_highlight_syntax(true);
        buffer.set_highlight_matching_brackets(true);
        buffer.set_style_scheme(scheme);
        let view = sourceview5::View::with_buffer(&buffer);
        view.add_css_class("sd-editor-text");
        view.set_monospace(true);
        view.set_show_line_numbers(true);
        view.set_highlight_current_line(true);
        view.set_auto_indent(true);
        view.set_indent_on_tab(true);
        view.set_smart_backspace(true);
        view.set_tab_width(4);
        view.set_insert_spaces_instead_of_tabs(true);
        view.set_left_margin(6);
        let page = gtk4::ScrolledWindow::new();
        page.set_child(Some(&view));
        page.set_vexpand(true);
        page.set_hexpand(true);

        let tab = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        let label = gtk4::Label::new(None);
        label.add_css_class("sd-editor-tab-label");
        let close = gtk4::Button::with_label("✕");
        close.add_css_class("term-btn");
        close.add_css_class("sd-editor-tab-close");
        close.set_tooltip_text(Some("Close (Ctrl+W)"));
        close.set_focus_on_click(false);
        tab.append(&label);
        tab.append(&close);

        let doc = Rc::new(Self {
            path: RefCell::new(None),
            buffer,
            view,
            page,
            tab,
            close,
            label,
            untitled,
            monitor: RefCell::new(None),
            on_disk_change: RefCell::new(None),
            on_title: RefCell::new(None),
            stale: Cell::new(false),
        });
        let weak = Rc::downgrade(&doc);
        doc.buffer.connect_modified_changed(move |_| {
            if let Some(doc) = weak.upgrade() {
                doc.refresh_title();
            }
        });
        doc.refresh_title();
        doc
    }

    /// The file's name, or "Untitled n".
    pub fn title(&self) -> String {
        match self.path.borrow().as_ref() {
            Some(path) => path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned()),
            None => format!("Untitled {}", self.untitled),
        }
    }

    pub fn is_modified(&self) -> bool {
        self.buffer.is_modified()
    }

    pub fn text(&self) -> String {
        let (start, end) = self.buffer.bounds();
        self.buffer.text(&start, &end, true).to_string()
    }

    fn refresh_title(&self) {
        let mark = if self.is_modified() { "● " } else { "" };
        self.label.set_text(&format!("{mark}{}", self.title()));
        let tip = self.path.borrow().as_ref().map_or_else(|| self.title(), |p| p.display().to_string());
        self.tab.set_tooltip_text(Some(&tip));
        if let Some(on_title) = self.on_title.borrow().clone() {
            on_title();
        }
    }

    /// Loads `path` into the buffer: not undoable, not modified, cursor at
    /// the top, the language guessed from the name.
    pub fn load(self: &Rc<Self>, path: &Path) -> Result<(), String> {
        let text = read(path)?;
        self.set_text_from_disk(&text);
        self.buffer.place_cursor(&self.buffer.start_iter());
        self.set_path(path);
        Ok(())
    }

    fn set_text_from_disk(&self, text: &str) {
        self.buffer.begin_irreversible_action();
        self.buffer.set_text(text);
        self.buffer.end_irreversible_action();
        self.buffer.set_modified(false);
        self.stale.set(false);
    }

    /// Reads the file again, keeping the cursor's line.
    pub fn reload(&self) -> Result<(), String> {
        let Some(path) = self.path.borrow().clone() else { return Ok(()) };
        let text = read(&path)?;
        let line = self.buffer.iter_at_mark(&self.buffer.get_insert()).line();
        self.set_text_from_disk(&text);
        if let Some(iter) = self.buffer.iter_at_line(line) {
            self.buffer.place_cursor(&iter);
        }
        Ok(())
    }

    /// Writes the buffer to its file.
    pub fn save(self: &Rc<Self>) -> Result<(), String> {
        let path = self.path.borrow().clone().ok_or("the file has no name yet: use Save as")?;
        self.write(&path)
    }

    /// Writes the buffer to `path` and makes that its file.
    pub fn save_as(self: &Rc<Self>, path: &Path) -> Result<(), String> {
        self.write(path)?;
        self.set_path(path);
        Ok(())
    }

    fn write(&self, path: &Path) -> Result<(), String> {
        let text = self.text();
        // Atomic, and keeps the file's permissions and owner.
        gio::File::for_path(path)
            .replace_contents(text.as_bytes(), None, false, gio::FileCreateFlags::NONE, None::<&gio::Cancellable>)
            .map_err(|e| format!("cannot save {}: {}", path.display(), e.message()))?;
        self.buffer.set_modified(false);
        self.stale.set(false);
        Ok(())
    }

    fn set_path(self: &Rc<Self>, path: &Path) {
        *self.path.borrow_mut() = Some(path.to_path_buf());
        let language = sourceview5::LanguageManager::default().guess_language(Some(path), None);
        self.buffer.set_language(language.as_ref());
        self.watch(path);
        self.refresh_title();
    }

    /// Notices another program changing the file: an unedited buffer
    /// reloads, an edited one asks (`on_disk_change`). Its own saves look
    /// like changes too; the text on disk then equals the buffer's.
    fn watch(self: &Rc<Self>, path: &Path) {
        let Ok(monitor) = gio::File::for_path(path).monitor_file(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>) else {
            return;
        };
        let weak = Rc::downgrade(self);
        let watched = path.to_path_buf();
        monitor.connect_changed(move |_, _, _, event| {
            if !matches!(event, gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Created) {
                return;
            }
            let Some(doc) = weak.upgrade() else { return };
            if doc.path.borrow().as_deref() != Some(watched.as_path()) {
                return;
            }
            let Ok(on_disk) = read(&watched) else { return };
            if on_disk == doc.text() {
                return;
            }
            if !doc.is_modified() {
                let _ = doc.reload();
            } else if !doc.stale.replace(true) {
                if let Some(ask) = doc.on_disk_change.borrow().clone() {
                    let doc = Rc::clone(&doc);
                    glib::idle_add_local_once(move || ask(&doc));
                }
            }
        });
        *self.monitor.borrow_mut() = Some(monitor);
    }

    /// Its file was renamed or moved (the tree's Rename).
    pub fn moved_to(self: &Rc<Self>, path: &Path) {
        self.set_path(path);
    }

    /// Stops watching its file (the tab is closing).
    pub fn forget(&self) {
        if let Some(monitor) = self.monitor.take() {
            monitor.cancel();
        }
    }

    /// Puts the cursor at the start of `line` (from 1) and shows it.
    pub fn go_to_line(&self, line: i32) {
        let last = self.buffer.line_count().max(1);
        if let Some(iter) = self.buffer.iter_at_line((line - 1).clamp(0, last - 1)) {
            self.buffer.place_cursor(&iter);
            let mark = self.buffer.get_insert();
            self.view.scroll_to_mark(&mark, 0.2, false, 0.0, 0.5);
        }
    }
}
