//! SD Editor's folder tree: the editor's folder, folders first, read as
//! they are expanded and kept up to date as files come and go. Each shown
//! folder is watched and read again on any change; only what changed is
//! applied, so expanded folders stay expanded. (GTK's DirectoryList stops
//! following a folder once a file in it is replaced by a rename, which is
//! how editors and git save files.) Hidden files show on request.
use gtk4::prelude::*;
use gtk4::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub struct Tree {
    pub widget: gtk4::Box,
    #[cfg_attr(not(test), allow(dead_code))]
    list: gtk4::ListView,
    show_hidden: Rc<Cell<bool>>,
    filter: gtk4::CustomFilter,
    #[cfg_attr(not(test), allow(dead_code))]
    folders: Folders,
}

/// A right click: the folder new files go in, the row's path, where.
type OnMenu = Rc<dyn Fn(PathBuf, Option<PathBuf>, &gtk4::Widget, f64, f64)>;

/// The folders shown now: their entries and their watches.
type Folders = Rc<RefCell<Vec<(glib::WeakRef<gio::ListStore>, gio::FileMonitor)>>>;

/// The file a row stands for.
fn file_of(info: &gio::FileInfo) -> Option<gio::File> {
    info.attribute_object("standard::file")?.downcast::<gio::File>().ok()
}

fn is_dir(info: &gio::FileInfo) -> bool {
    info.file_type() == gio::FileType::Directory
}

/// Folders first, then by name ignoring case, then exactly.
fn key(info: &gio::FileInfo) -> (bool, String, String) {
    let name = info.name().to_string_lossy().into_owned();
    (!is_dir(info), name.to_lowercase(), name)
}

/// One entry of `dir`, as the tree shows it.
fn entry(path: &Path) -> gio::FileInfo {
    let info = gio::FileInfo::new();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // A link counts as what it points to.
    let dir = std::fs::metadata(path).is_ok_and(|m| m.is_dir());
    info.set_name(Path::new(&name));
    info.set_display_name(&name);
    info.set_is_hidden(name.starts_with('.'));
    info.set_file_type(if dir { gio::FileType::Directory } else { gio::FileType::Regular });
    info.set_attribute_object("standard::file", &gio::File::for_path(path));
    let icon: gio::Icon = if dir {
        gio::ThemedIcon::new("folder-symbolic").upcast()
    } else {
        gio::content_type_get_symbolic_icon(&gio::content_type_guess(Some(path), None).0)
    };
    info.set_symbolic_icon(&icon);
    info
}

/// `dir`'s entries, sorted.
fn read(dir: &Path) -> Vec<gio::FileInfo> {
    let mut found: Vec<gio::FileInfo> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| entry(&e.path())).collect();
    found.sort_by_key(key);
    found
}

/// Makes `store` hold `dir`'s entries with the fewest changes: entries that
/// stay keep their objects (and so their rows and expanded state).
fn sync(store: &gio::ListStore, dir: &Path) {
    let wanted = read(dir);
    let mut at = 0;
    for want in &wanted {
        loop {
            let current = (at < store.n_items()).then(|| store.item(at).and_downcast::<gio::FileInfo>()).flatten();
            match current.map(|c| key(&c).cmp(&key(want))) {
                Some(std::cmp::Ordering::Less) => store.remove(at),
                Some(std::cmp::Ordering::Equal) => {
                    at += 1;
                    break;
                }
                _ => {
                    store.insert(at, want);
                    at += 1;
                    break;
                }
            }
        }
    }
    while store.n_items() > at {
        store.remove(at);
    }
}

/// `dir`'s entries as a model, watched for changes while it is shown.
fn children(dir: &Path, filter: &gtk4::CustomFilter, folders: &Folders) -> gio::ListModel {
    let store = gio::ListStore::new::<gio::FileInfo>();
    sync(&store, dir);
    folders.borrow_mut().retain(|(store, _)| store.upgrade().is_some());
    if let Ok(monitor) = gio::File::for_path(dir).monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>) {
        let (weak, dir, queued) = (store.downgrade(), dir.to_path_buf(), Rc::new(Cell::new(false)));
        monitor.connect_changed(move |_, _, _, _| {
            // A burst of events (a save, a checkout) is read once.
            if queued.replace(true) {
                return;
            }
            let (weak, dir, queued) = (weak.clone(), dir.clone(), Rc::clone(&queued));
            glib::timeout_add_local_once(std::time::Duration::from_millis(80), move || {
                queued.set(false);
                if let Some(store) = weak.upgrade() {
                    sync(&store, &dir);
                }
            });
        });
        folders.borrow_mut().push((store.downgrade(), monitor));
    }
    gtk4::FilterListModel::new(Some(store), Some(filter.clone())).upcast()
}

/// The path of the item a list row shows.
fn path_of(item: Option<glib::Object>) -> Option<(PathBuf, bool)> {
    let row = item?.downcast::<gtk4::TreeListRow>().ok()?;
    let info = row.item()?.downcast::<gio::FileInfo>().ok()?;
    Some((file_of(&info)?.path()?, is_dir(&info)))
}

impl Tree {
    /// The tree of `root`. `on_open` gets a file the user clicked; `on_menu`
    /// a right click: the folder new files would go in, the row's path (none
    /// on empty space), and where the click was in `widget`.
    pub fn new(
        root: &Path,
        on_open: Rc<dyn Fn(PathBuf)>,
        on_menu: OnMenu,
    ) -> Rc<Self> {
        let show_hidden = Rc::new(Cell::new(false));
        let filter = gtk4::CustomFilter::new({
            let show_hidden = Rc::clone(&show_hidden);
            move |item| {
                let Some(info) = item.downcast_ref::<gio::FileInfo>() else { return false };
                show_hidden.get() || !(info.is_hidden() || info.name().to_string_lossy().starts_with('.'))
            }
        });
        let folders: Folders = Rc::new(RefCell::new(Vec::new()));
        let model = gtk4::TreeListModel::new(children(root, &filter, &folders), false, false, {
            let (filter, folders) = (filter.clone(), Rc::clone(&folders));
            move |item| {
                let info = item.downcast_ref::<gio::FileInfo>()?;
                let dir = file_of(info)?.path()?;
                is_dir(info).then(|| children(&dir, &filter, &folders))
            }
        });
        let selection = gtk4::SingleSelection::new(Some(model));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);

        let factory = gtk4::SignalListItemFactory::new();
        factory.connect_setup({
            let on_menu = Rc::clone(&on_menu);
            let root = root.to_path_buf();
            move |_, item| {
                let Some(item) = item.downcast_ref::<gtk4::ListItem>() else { return };
                let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
                row.append(&gtk4::Image::new());
                let name = gtk4::Label::new(None);
                name.set_xalign(0.0);
                name.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
                row.append(&name);
                let expander = gtk4::TreeExpander::new();
                expander.set_child(Some(&row));
                expander.set_indent_for_icon(true);
                // A right click on a row: its menu, for that folder or file.
                let click = gtk4::GestureClick::new();
                click.set_button(3);
                let weak = item.downgrade();
                let (on_menu, root) = (Rc::clone(&on_menu), root.clone());
                click.connect_pressed(move |gesture, _, x, y| {
                    let Some(item) = weak.upgrade() else { return };
                    let Some((path, dir)) = path_of(item.item()) else { return };
                    gesture.set_state(gtk4::EventSequenceState::Claimed);
                    let folder = if dir { path.clone() } else { path.parent().map_or_else(|| root.clone(), Path::to_path_buf) };
                    if let Some(widget) = gesture.widget() {
                        on_menu(folder, Some(path), &widget, x, y);
                    }
                });
                expander.add_controller(click);
                item.set_child(Some(&expander));
            }
        });
        factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk4::ListItem>() else { return };
            let Some(row) = item.item().and_downcast::<gtk4::TreeListRow>() else { return };
            let Some(expander) = item.child().and_downcast::<gtk4::TreeExpander>() else { return };
            expander.set_list_row(Some(&row));
            let Some(info) = row.item().and_downcast::<gio::FileInfo>() else { return };
            let Some(line) = expander.child() else { return };
            if let Some(icon) = line.first_child().and_downcast::<gtk4::Image>() {
                icon.set_from_gicon(&info.symbolic_icon().unwrap_or_else(|| gio::ThemedIcon::new(if is_dir(&info) { "folder-symbolic" } else { "text-x-generic-symbolic" }).upcast()));
            }
            if let Some(name) = line.last_child().and_downcast::<gtk4::Label>() {
                name.set_text(&info.display_name());
            }
        });

        let list = gtk4::ListView::new(Some(selection.clone()), Some(factory));
        list.add_css_class("sd-editor-tree");
        list.set_single_click_activate(true);
        list.connect_activate(move |_, position| {
            let Some(row) = selection.item(position).and_downcast::<gtk4::TreeListRow>() else { return };
            let Some(info) = row.item().and_downcast::<gio::FileInfo>() else { return };
            if is_dir(&info) {
                row.set_expanded(!row.is_expanded());
            } else if let Some(path) = file_of(&info).and_then(|f| f.path()) {
                on_open(path);
            }
        });

        let title = gtk4::Label::new(Some(&root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned())));
        title.add_css_class("sd-editor-tree-title");
        title.set_xalign(0.0);
        title.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
        title.set_tooltip_text(Some(&root.display().to_string()));
        let scroller = gtk4::ScrolledWindow::new();
        scroller.set_child(Some(&list));
        scroller.set_vexpand(true);
        scroller.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        let widget = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        widget.add_css_class("sd-editor-side");
        widget.append(&title);
        widget.append(&scroller);
        // A right click on empty space: the menu for the editor's folder.
        let click = gtk4::GestureClick::new();
        click.set_button(3);
        let root_path = root.to_path_buf();
        click.connect_pressed(move |gesture, _, x, y| {
            if let Some(widget) = gesture.widget() {
                on_menu(root_path.clone(), None, &widget, x, y);
            }
        });
        scroller.add_controller(click);

        Rc::new(Self { widget, list, show_hidden, filter, folders })
    }

    pub fn shows_hidden(&self) -> bool {
        self.show_hidden.get()
    }

    pub fn set_show_hidden(&self, show: bool) {
        if self.show_hidden.replace(show) != show {
            self.filter.changed(if show { gtk4::FilterChange::LessStrict } else { gtk4::FilterChange::MoreStrict });
        }
    }

    /// The paths the tree shows now, top to bottom (for tests).
    #[cfg(test)]
    pub fn shown(&self) -> Vec<PathBuf> {
        let Some(model) = self.list.model() else { return Vec::new() };
        (0..model.n_items()).filter_map(|i| path_of(model.item(i)).map(|(p, _)| p)).collect()
    }

    /// How many folders are watched now (for tests).
    #[cfg(test)]
    pub fn watched(&self) -> usize {
        self.folders.borrow().iter().filter(|(store, _)| store.upgrade().is_some()).count()
    }

    /// Expands the folder row for `path` (for tests).
    #[cfg(test)]
    pub fn expand(&self, path: &Path) {
        let Some(model) = self.list.model() else { return };
        for i in 0..model.n_items() {
            let Some(row) = model.item(i).and_downcast::<gtk4::TreeListRow>() else { continue };
            if path_of(Some(row.clone().upcast())).is_some_and(|(p, _)| p == path) {
                row.set_expanded(true);
            }
        }
    }
}
