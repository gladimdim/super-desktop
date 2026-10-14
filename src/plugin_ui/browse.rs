//! Settings → Plugins → Discover: the public plugin list
//! (`plugins::registry`) as a list of cards, each a square preview picture
//! (a quarter of the card's width) beside its text, with a search, category
//! chips (all categories: grouped under each), and one button per plugin:
//! Install (from its GitHub repository; it then waits for approval under
//! Installed), or where it stands. The list shows at once from the last
//! fetch; the list and the previews refresh off the GTK thread.
use super_desktop::plugins::install::{self, Places};
use super_desktop::plugins::registry::{self, Compatibility, Entry, Order};
use gtk4::prelude::*;
use gtk4::{gio, glib, Align, Orientation};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

/// Of a card's four equal columns, the picture takes one.
const CARD_COLUMNS: i32 = 4;

mod imp {
    use gtk4::glib;
    use gtk4::prelude::*;
    use gtk4::subclass::prelude::*;

    /// Holds one child and asks for a height equal to its width.
    #[derive(Default)]
    pub struct SquareBin;

    #[glib::object_subclass]
    impl ObjectSubclass for SquareBin {
        const NAME: &'static str = "SdPluginSquareBin";
        type Type = super::SquareBin;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for SquareBin {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for SquareBin {
        fn request_mode(&self) -> gtk4::SizeRequestMode {
            gtk4::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk4::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                // Any width; as tall as it is wide.
                gtk4::Orientation::Horizontal => (1, for_size.max(1), -1, -1),
                _ => {
                    let side = if for_size > 0 { for_size } else { 1 };
                    (side, side, -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.obj().first_child() {
                child.allocate(width, height, baseline, None);
            }
        }
    }
}

gtk4::glib::wrapper! {
    /// A square: its child is as tall as it is wide (or taller when the row
    /// it is in is).
    pub struct SquareBin(ObjectSubclass<imp::SquareBin>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl SquareBin {
    fn new(child: &impl IsA<gtk4::Widget>) -> Self {
        let bin: Self = glib::Object::new();
        child.set_parent(&bin);
        bin
    }
}

fn text(text: &str, class: &str) -> gtk4::Label {
    let label = gtk4::Label::new(Some(text));
    label.add_css_class(class);
    label.set_xalign(0.0);
    label
}

/// Whether a plugin with this id is here, and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Here {
    No,
    Installed,
    WaitingApproval,
}

pub fn here(places: &Places, id: &str) -> Here {
    if !places.plugins.join(id).exists() {
        Here::No
    } else if install::approved(places, id) {
        Here::Installed
    } else {
        Here::WaitingApproval
    }
}

/// "top bar, popup" from manifest slot names, without repeats.
pub fn slots_text<'a>(slots: impl IntoIterator<Item = &'a str>) -> String {
    let mut names: Vec<&str> = Vec::new();
    for slot in slots {
        let name = match slot {
            "toolbar.item" => "top bar",
            "dock.left" => "left dock",
            "dock.right" => "right dock",
            "settings.page" => "settings",
            "fullscreen" => "full screen",
            other => other,
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names.join(", ")
}

/// A picture of a plugin, or a quiet 🧩 when it has none (yet).
pub fn preview_box(height: i32) -> (gtk4::Overlay, gtk4::Picture) {
    let frame = gtk4::Overlay::new();
    frame.add_css_class("plugin-card-preview");
    frame.set_size_request(-1, height);
    frame.set_overflow(gtk4::Overflow::Hidden);
    let placeholder = gtk4::Label::new(Some("🧩"));
    placeholder.add_css_class("plugin-card-placeholder");
    frame.set_child(Some(&placeholder));
    let picture = gtk4::Picture::new();
    picture.set_content_fit(gtk4::ContentFit::Cover);
    picture.set_can_shrink(true);
    picture.set_visible(false);
    frame.add_overlay(&picture);
    (frame, picture)
}

/// The middle square of a picture (previews are 2:1; the list shows squares).
pub fn square_texture(path: &std::path::Path) -> Option<gtk4::gdk::Texture> {
    let texture = gtk4::gdk::Texture::from_filename(path).ok()?;
    let (width, height) = (texture.width() as usize, texture.height() as usize);
    let side = width.min(height);
    if side == 0 {
        return None;
    }
    let stride = width * 4;
    let mut data = vec![0u8; stride * height];
    // Downloads as B8G8R8A8 premultiplied.
    texture.download(&mut data, stride);
    let (left, top) = ((width - side) / 2, (height - side) / 2);
    let mut square = Vec::with_capacity(side * side * 4);
    for row in top..top + side {
        let start = row * stride + left * 4;
        square.extend_from_slice(&data[start..start + side * 4]);
    }
    Some(gtk4::gdk::MemoryTexture::new(side as i32, side as i32, gtk4::gdk::MemoryFormat::B8g8r8a8Premultiplied,
        &glib::Bytes::from_owned(square), side * 4).upcast())
}

/// A square picture as wide as its column: a transparent square until the
/// preview is there (GtkPicture keeps its paintable's proportions), with a
/// quiet 🧩 over it.
fn square_preview(texture: Option<&gtk4::gdk::Texture>) -> gtk4::Overlay {
    let frame = gtk4::Overlay::new();
    frame.add_css_class("plugin-card-preview");
    frame.set_overflow(gtk4::Overflow::Hidden);
    let picture = gtk4::Picture::new();
    picture.set_content_fit(gtk4::ContentFit::Cover);
    picture.set_can_shrink(true);
    picture.set_hexpand(true);
    match texture {
        Some(texture) => picture.set_paintable(Some(texture)),
        None => {
            let blank = gtk4::gdk::MemoryTexture::new(1, 1, gtk4::gdk::MemoryFormat::R8g8b8a8, &glib::Bytes::from_static(&[0, 0, 0, 0]), 4);
            picture.set_paintable(Some(&blank));
            let placeholder = gtk4::Label::new(Some("🧩"));
            placeholder.add_css_class("plugin-card-placeholder");
            frame.add_overlay(&placeholder);
        }
    }
    frame.set_child(Some(&picture));
    frame
}

pub fn show_preview(picture: &gtk4::Picture, path: &std::path::Path) {
    picture.set_filename(Some(path));
    picture.set_visible(true);
    // The 🧩 under it goes.
    if let Some(placeholder) = picture.parent().and_downcast::<gtk4::Overlay>().and_then(|frame| frame.child()) {
        placeholder.set_visible(false);
    }
}

pub struct Browser {
    pub widget: gtk4::Box,
    search: gtk4::SearchEntry,
    chips: gtk4::FlowBox,
    status: gtk4::Label,
    results: gtk4::Box,
    entries: RefCell<Vec<Entry>>,
    categories: RefCell<Vec<String>>,
    /// The chosen category; `None` for all.
    category: RefCell<Option<String>>,
    /// Previews by plugin id, as they arrive.
    previews: RefCell<BTreeMap<String, Option<PathBuf>>>,
    /// Their middle squares, made once.
    squares: RefCell<BTreeMap<String, gtk4::gdk::Texture>>,
    loading: Cell<bool>,
}

impl Browser {
    pub fn new() -> Rc<Self> {
        let widget = gtk4::Box::new(Orientation::Vertical, 10);
        widget.add_css_class("plugin-discover");
        let bar = gtk4::Box::new(Orientation::Horizontal, 6);
        let search = gtk4::SearchEntry::new();
        search.set_placeholder_text(Some("Search plugins"));
        search.set_hexpand(true);
        search.add_css_class("plugin-search");
        bar.append(&search);
        let refresh = gtk4::Button::with_label("↻");
        refresh.add_css_class("plugin-icon-button");
        refresh.set_tooltip_text(Some("Check the list again"));
        bar.append(&refresh);
        widget.append(&bar);
        let chips = gtk4::FlowBox::new();
        chips.set_selection_mode(gtk4::SelectionMode::None);
        chips.set_max_children_per_line(12);
        chips.set_row_spacing(6);
        chips.set_column_spacing(6);
        chips.set_homogeneous(false);
        widget.append(&chips);
        let status = text("", "plugin-subtle");
        status.set_wrap(true);
        status.set_visible(false);
        widget.append(&status);
        let results = gtk4::Box::new(Orientation::Vertical, 14);
        widget.append(&results);
        let footer = text("From github.com/gladimdim/super-desktop-plugins. Plugins are not reviewed by SUPER DESKTOP: what you install waits for your approval under Installed.", "plugin-subtle");
        footer.set_wrap(true);
        widget.append(&footer);

        let this = Rc::new(Self {
            widget,
            search,
            chips,
            status,
            results,
            entries: RefCell::new(Vec::new()),
            categories: RefCell::new(Vec::new()),
            category: RefCell::new(None),
            previews: RefCell::new(BTreeMap::new()),
            squares: RefCell::new(BTreeMap::new()),
            loading: Cell::new(false),
        });
        let weak = Rc::downgrade(&this);
        this.search.connect_search_changed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.render();
            }
        });
        let weak = Rc::downgrade(&this);
        refresh.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.fetch(true);
            }
        });
        // The last list at once; a fresh one when it arrives.
        if let Some((entries, _)) = registry::cached() {
            this.set_entries(entries);
        }
        this.fetch(false);
        this
    }

    fn say(&self, message: Option<&str>, error: bool) {
        self.status.set_visible(message.is_some());
        self.status.set_text(message.unwrap_or(""));
        if error {
            self.status.add_css_class("plugin-error");
        } else {
            self.status.remove_css_class("plugin-error");
        }
    }

    fn set_entries(self: &Rc<Self>, entries: Vec<Entry>) {
        let categories = registry::categories(&entries);
        if *self.categories.borrow() != categories || self.chips.first_child().is_none() {
            *self.categories.borrow_mut() = categories;
            let keep = self.category.borrow().clone().filter(|c| self.categories.borrow().contains(c));
            *self.category.borrow_mut() = keep;
            self.build_chips();
        }
        *self.entries.borrow_mut() = entries;
        self.render();
        self.load_previews(false);
    }

    /// One chip per category, and All.
    fn build_chips(self: &Rc<Self>) {
        while let Some(child) = self.chips.first_child() {
            self.chips.remove(&child);
        }
        let mut first: Option<gtk4::ToggleButton> = None;
        let names: Vec<Option<String>> = std::iter::once(None).chain(self.categories.borrow().iter().cloned().map(Some)).collect();
        for name in names {
            let chip = gtk4::ToggleButton::with_label(&name.as_deref().map_or_else(|| "All".to_owned(), capitalize));
            chip.add_css_class("plugin-chip");
            match &first {
                Some(first) => chip.set_group(Some(first)),
                None => first = Some(chip.clone()),
            }
            chip.set_active(*self.category.borrow() == name);
            let weak = Rc::downgrade(self);
            chip.connect_toggled(move |chip| {
                if !chip.is_active() {
                    return;
                }
                if let Some(this) = weak.upgrade() {
                    *this.category.borrow_mut() = name.clone();
                    this.render();
                }
            });
            self.chips.insert(&chip, -1);
        }
    }

    /// Gets the list again, off the GTK thread (`again`: previews too).
    pub fn fetch(self: &Rc<Self>, again: bool) {
        if self.loading.replace(true) {
            return;
        }
        if self.entries.borrow().is_empty() {
            self.say(Some("Getting the plugin list…"), false);
        }
        let weak = Rc::downgrade(self);
        let handle = gio::spawn_blocking(registry::fetch);
        glib::spawn_future_local(async move {
            let result = handle.await.unwrap_or_else(|_| Err("the request failed".into()));
            let Some(this) = weak.upgrade() else { return };
            this.loading.set(false);
            match result {
                Ok((entries, _)) => {
                    this.say(None, false);
                    this.set_entries(entries);
                    if again {
                        this.load_previews(true);
                    }
                }
                Err(_) if !this.entries.borrow().is_empty() => {
                    this.say(Some("Offline: this is the list from the last check."), false);
                }
                Err(e) => this.say(Some(&e), true),
            }
        });
    }

    /// Downloads the previews not here yet, off the GTK thread.
    fn load_previews(self: &Rc<Self>, again: bool) {
        let wanted: Vec<Entry> = self.entries.borrow().iter()
            .filter(|e| again || !self.previews.borrow().contains_key(&e.id))
            .cloned()
            .collect();
        if wanted.is_empty() {
            return;
        }
        for entry in &wanted {
            self.previews.borrow_mut().insert(entry.id.clone(), None);
        }
        let weak = Rc::downgrade(self);
        let handle = gio::spawn_blocking(move || {
            wanted.iter().map(|e| (e.id.clone(), registry::preview(e, again))).collect::<Vec<_>>()
        });
        glib::spawn_future_local(async move {
            let Ok(found) = handle.await else { return };
            let Some(this) = weak.upgrade() else { return };
            let changed = found.iter().any(|(_, path)| path.is_some());
            for (id, _) in &found {
                this.squares.borrow_mut().remove(id);
            }
            this.previews.borrow_mut().extend(found);
            if changed {
                this.render();
            }
        });
    }

    /// Shows the cards that match the search and category.
    pub fn render(self: &Rc<Self>) {
        while let Some(child) = self.results.first_child() {
            self.results.remove(&child);
        }
        let entries = self.entries.borrow().clone();
        if entries.is_empty() {
            return;
        }
        let category = self.category.borrow().clone();
        // All categories: grouped under each plugin's first one.
        let order = if category.is_none() { Order::Category } else { Order::Name };
        let found = registry::select(&entries, self.search.text().as_str(), category.as_deref(), order);
        if found.is_empty() {
            self.results.append(&text("No plugin matches.", "plugin-subtle"));
            return;
        }
        let mut groups: Vec<(Option<String>, Vec<&Entry>)> = Vec::new();
        for (entry, under) in found {
            match groups.last_mut() {
                Some((group, list)) if *group == under => list.push(entry),
                _ => groups.push((under, vec![entry])),
            }
        }
        for (under, list) in groups {
            if let Some(under) = under {
                self.results.append(&text(&capitalize(&under), "plugin-group-title"));
            }
            let column = gtk4::Box::new(Orientation::Vertical, 10);
            for entry in list {
                column.append(&self.card(entry));
            }
            self.results.append(&column);
        }
    }

    /// The square of a plugin's preview, made the first time it is shown.
    fn square(&self, id: &str) -> Option<gtk4::gdk::Texture> {
        if let Some(texture) = self.squares.borrow().get(id) {
            return Some(texture.clone());
        }
        let path = self.previews.borrow().get(id).cloned().flatten()?;
        let texture = square_texture(&path)?;
        self.squares.borrow_mut().insert(id.to_owned(), texture.clone());
        Some(texture)
    }

    /// The picture in the first of four equal columns, the text in the rest.
    fn card(self: &Rc<Self>, entry: &Entry) -> gtk4::Grid {
        let card = gtk4::Grid::new();
        card.add_css_class("plugin-card");
        card.set_column_homogeneous(true);
        card.set_overflow(gtk4::Overflow::Hidden);
        card.attach(&SquareBin::new(&square_preview(self.square(&entry.id).as_ref())), 0, 0, 1, 1);

        let body = gtk4::Box::new(Orientation::Vertical, 4);
        body.add_css_class("plugin-card-body");
        body.set_vexpand(true);
        let title = gtk4::Box::new(Orientation::Horizontal, 6);
        let name = text(&entry.name, "plugin-card-name");
        name.set_hexpand(true);
        name.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        title.append(&name);
        match entry.compatibility() {
            Compatibility::Ok => {}
            Compatibility::MadeForNewer(v) => {
                let badge = text(&format!("{v}+"), "plugin-badge");
                badge.set_tooltip_text(Some(&format!(
                    "Made for SUPER DESKTOP {v} or newer; this is {}. It may not load until you update.",
                    env!("CARGO_PKG_VERSION")
                )));
                title.append(&badge);
            }
            Compatibility::NeedsApi(api) => {
                let badge = text("Update needed", "plugin-badge");
                badge.set_tooltip_text(Some(&format!("Needs plugin API {api}: update SUPER DESKTOP to install it.")));
                title.append(&badge);
            }
        }
        body.append(&title);
        let description = text(&entry.description, "plugin-card-text");
        description.set_wrap(true);
        description.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
        description.set_lines(4);
        description.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        description.set_tooltip_text(Some(&entry.description));
        description.set_valign(Align::Start);
        description.set_vexpand(true);
        body.append(&description);
        let mut meta = format!("by {}", entry.author);
        if !entry.slots.is_empty() {
            meta.push_str(&format!(" · {}", slots_text(entry.slots.iter().map(String::as_str))));
        }
        let meta = text(&meta, "plugin-card-meta");
        meta.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        body.append(&meta);

        let footer = gtk4::Box::new(Orientation::Horizontal, 6);
        footer.set_margin_top(6);
        let note = text("", "plugin-card-meta");
        note.set_wrap(true);
        note.set_visible(false);
        let action = gtk4::Button::new();
        action.add_css_class("plugin-action");
        match here(&Places::current(), &entry.id) {
            Here::Installed => {
                action.set_label("✓ Installed");
                action.set_sensitive(false);
            }
            Here::WaitingApproval => {
                action.set_label("Approve under Installed");
                action.set_sensitive(false);
            }
            Here::No => {
                action.set_label("Install");
                action.set_tooltip_text(Some(&format!("From {}; it runs once you approve it", entry.web_url())));
                let installable = !matches!(entry.compatibility(), Compatibility::NeedsApi(_));
                action.set_sensitive(installable);
                if installable {
                    action.add_css_class("plugin-action-primary");
                }
                let (url, id, note) = (entry.git_url(), entry.id.clone(), note.clone());
                action.connect_clicked(move |action| {
                    action.set_sensitive(false);
                    action.set_label("Installing…");
                    let (url, id, places) = (url.clone(), id.clone(), Places::current());
                    let handle = gio::spawn_blocking(move || install::install_as(&places, &url, Some(&id)).map(|_| ()));
                    let (note, action) = (note.clone(), action.clone());
                    glib::spawn_future_local(async move {
                        match handle.await.unwrap_or_else(|_| Err("the install failed".into())) {
                            Ok(()) => {
                                action.remove_css_class("plugin-action-primary");
                                action.set_label("Approve under Installed");
                                note.remove_css_class("plugin-error");
                                note.set_text("Installed. It runs once you approve it under Installed.");
                            }
                            Err(e) => {
                                action.set_label("Install");
                                action.set_sensitive(true);
                                note.add_css_class("plugin-error");
                                note.set_text(&e);
                            }
                        }
                        note.set_visible(true);
                    });
                });
            }
        }
        footer.append(&action);
        let page = gtk4::Button::with_label("GitHub ↗");
        page.add_css_class("plugin-link");
        page.set_tooltip_text(Some(&entry.web_url()));
        let url = entry.web_url();
        page.connect_clicked(move |_| {
            gtk4::UriLauncher::new(&url).launch(None::<&gtk4::Window>, gio::Cancellable::NONE, |_| {});
            // The browser opens under the overlay.
            if let Some(hooks) = super::api::hooks() {
                (hooks.hide)();
            }
        });
        footer.append(&page);
        body.append(&footer);
        body.append(&note);
        card.attach(&body, 1, 0, CARD_COLUMNS - 1, 1);
        card
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugins_browse_lists_filters_and_installs() {
        if std::process::Command::new("git").arg("--version").output().is_err() {
            eprintln!("skipped: git is not installed");
            return;
        }
        crate::gtk_test::run_in_child_process("plugin_ui::browse::tests::browse_inner");
    }

    #[test]
    fn browse_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let root = std::env::temp_dir().join(format!("sd-plugins-browse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("SD_PLUGINS_DIR", root.join("plugins"));
        std::env::set_var("SD_PLUGINS_STATE_DIR", root.join("state"));
        // Repositories under a local base instead of github.com.
        let repos = root.join("repos");
        let make_repo = |owner_name: &str, id: &str| {
            let dir = repos.join(owner_name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("plugin.json"), serde_json::json!({"id": id, "api": 1, "surfaces": [
                {"id": "main", "slot": "dock.left", "qml": "Dock.qml"}]}).to_string()).unwrap();
            std::fs::write(dir.join("Dock.qml"), "import QtQuick\nRectangle { color: \"red\" }\n").unwrap();
            for args in [&["init", "-q"][..], &["add", "."], &["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-qm", "first"]] {
                assert!(std::process::Command::new("git").arg("-C").arg(&dir).args(args).status().unwrap().success());
            }
        };
        make_repo("me/clock", "clock");
        make_repo("me/liar", "something-else");
        std::env::set_var("SD_PLUGIN_REPO_BASE", format!("file://{}", repos.display()));
        let list = root.join("registry.json");
        std::fs::write(&list, serde_json::json!({"registryVersion": 2, "plugins": [
            {"id": "clock", "name": "Clock", "description": "A dock clock", "repo": "me/clock", "tags": ["time"], "author": "me", "api": 1, "minHost": "1.0.0", "slots": ["dock.left"]},
            {"id": "liar", "name": "Liar", "description": "Says one id, holds another", "repo": "me/liar", "tags": ["games"], "author": "me", "api": 1, "minHost": "1.0.0"},
            {"id": "future", "name": "Future", "description": "Needs a newer API", "repo": "me/future", "tags": ["games"], "author": "me", "api": 9, "minHost": "1.0.0"}
        ]}).to_string()).unwrap();
        std::env::set_var("SD_PLUGIN_REGISTRY", &list);

        gtk4::init().unwrap();
        crate::styles::apply_styles();
        // One preview.png, served like raw.githubusercontent.com.
        let raw = root.join("raw");
        std::fs::create_dir_all(raw.join("me/clock/HEAD")).unwrap();
        // 2:1 like real previews: a blue left half, an orange right half.
        let pixels: Vec<u8> = (0..100 * 200).flat_map(|i| if i % 200 < 100 { [40u8, 90, 200, 255] } else { [230, 140, 30, 255] }).collect();
        let png = gtk4::gdk::MemoryTexture::new(200, 100, gtk4::gdk::MemoryFormat::R8g8b8a8,
            &glib::Bytes::from_owned(pixels), 800).save_to_png_bytes();
        std::fs::write(raw.join("me/clock/HEAD/preview.png"), &png).unwrap();
        std::env::set_var("SD_PLUGIN_RAW_BASE", format!("file://{}", raw.display()));

        let browser = Browser::new();
        let window = gtk4::Window::new();
        window.set_default_size(900, 800);
        // On the settings panel's background, as in the overlay.
        let panel = gtk4::Box::new(Orientation::Vertical, 0);
        panel.add_css_class("plugin-shot");
        let provider = gtk4::CssProvider::new();
        provider.load_from_string(&format!(".plugin-shot {{ background-color: {}; padding: 16px; }}", crate::theme::current_theme().rgba_dark_bg(0.97)));
        gtk4::style_context_add_provider_for_display(&gtk4::gdk::Display::default().unwrap(), &provider, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
        panel.append(&browser.widget);
        let scroller = gtk4::ScrolledWindow::new();
        scroller.set_child(Some(&panel));
        window.set_child(Some(&scroller));
        window.present();

        let labels = |class: &str| crate::gtk_test::descendants(browser.results.upcast_ref()).into_iter()
            .filter_map(|w| w.downcast::<gtk4::Label>().ok())
            .filter(|l| l.has_css_class(class) && l.is_visible())
            .map(|l| l.text().to_string())
            .collect::<Vec<_>>();
        let names = || labels("plugin-card-name");
        let buttons = |text: &str| crate::gtk_test::descendants(browser.results.upcast_ref()).into_iter()
            .filter_map(|w| w.downcast::<gtk4::Button>().ok())
            .filter(|b| b.label().as_deref() == Some(text))
            .collect::<Vec<_>>();
        let chip = |text: &str| crate::gtk_test::descendants(browser.chips.upcast_ref()).into_iter()
            .filter_map(|w| w.downcast::<gtk4::ToggleButton>().ok())
            .find(|b| b.label().as_deref() == Some(text))
            .unwrap();
        // Previews shown (not the 1-pixel blank of a card without one).
        let pictures = || crate::gtk_test::descendants(browser.results.upcast_ref()).into_iter()
            .filter_map(|w| w.downcast::<gtk4::Picture>().ok())
            .filter(|p| p.paintable().is_some_and(|t| t.intrinsic_width() > 1))
            .count();

        // All categories: grouped, the chips from the tags.
        crate::gtk_test::pump_until("the cards", || names().len() == 3);
        assert_eq!(names(), ["Future", "Liar", "Clock"]);
        assert_eq!(labels("plugin-group-title"), ["Games", "Time"]);
        assert!(chip("All").is_active());
        crate::gtk_test::pump_until("the one preview", || pictures() == 1);
        // Square, a quarter of the card's width.
        crate::gtk_test::pump(200);
        let card = crate::gtk_test::descendants(browser.results.upcast_ref()).into_iter()
            .find(|w| w.has_css_class("plugin-card")).unwrap();
        let picture = crate::gtk_test::descendants(&card).into_iter().find_map(|w| w.downcast::<gtk4::Picture>().ok()).unwrap();
        let (pw, ph, cw) = (picture.width(), picture.height(), card.width());
        assert!((pw - cw / 4).abs() <= 2, "picture {pw} of card {cw}");
        assert!(ph >= pw - 1, "picture {pw}x{ph} is square or taller (it fills the row)");
        // A search; a category chip.
        browser.search.set_text("dock");
        crate::gtk_test::pump_until("the search", || names() == ["Clock"]);
        browser.search.set_text("");
        crate::gtk_test::pump_until("the search cleared", || names().len() == 3);
        chip("Games").set_active(true);
        assert_eq!(names(), ["Future", "Liar"]);
        assert!(labels("plugin-group-title").is_empty());
        chip("All").set_active(true);
        // The API too new shows a badge and cannot be installed.
        assert_eq!(labels("plugin-badge"), ["Update needed"]);
        assert_eq!(buttons("Install").iter().filter(|b| b.is_sensitive()).count(), 2);

        // SD_UI_SHOT=file.png saves a picture of the view, to look at it.
        if let Ok(out) = std::env::var("SD_UI_SHOT") {
            crate::gtk_test::pump(300);
            crate::gtk_test::save_png(&panel, std::path::Path::new(&out));
        }

        // Install clones, then the plugin waits for approval.
        let install_clock = buttons("Install").into_iter().find(|b| b.tooltip_text().is_some_and(|t| t.contains("me/clock"))).unwrap();
        install_clock.emit_clicked();
        crate::gtk_test::pump_until("the install", || install_clock.label().as_deref() == Some("Approve under Installed"));
        let places = Places::current();
        assert!(places.plugins.join("clock/plugin.json").is_file());
        assert_eq!(here(&places, "clock"), Here::WaitingApproval);
        // An entry whose repository holds another id is refused, nothing installed.
        let install_liar = buttons("Install").into_iter().find(|b| b.tooltip_text().is_some_and(|t| t.contains("me/liar"))).unwrap();
        install_liar.emit_clicked();
        crate::gtk_test::pump_until("the refusal", || install_liar.is_sensitive());
        assert!(labels("plugin-card-meta").iter().any(|t| t.contains("not \"liar\"")), "the refusal says why");
        assert!(!places.plugins.join("something-else").exists() && !places.plugins.join("liar").exists());
        browser.render();
        assert_eq!(buttons("Approve under Installed").len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
