//! Settings → Plugins → Get plugins: the public plugin list
//! (`plugins::registry`), searched by text, filtered by category, ordered by
//! name or grouped by category, each plugin with Install (from its GitHub
//! repository, then it waits for approval in the list below) and a link to
//! its page. The list shows at once from the last fetch and refreshes off the
//! GTK thread.
use super_desktop::plugins::install::{self, Places};
use super_desktop::plugins::registry::{self, Compatibility, Entry, Order};
use gtk4::prelude::*;
use gtk4::{gio, glib, Align, Orientation};
use std::cell::RefCell;
use std::rc::Rc;

const ALL: &str = "All categories";
const ORDERS: [&str; 2] = ["By category", "By name"];

fn label(text: &str, class: &str) -> gtk4::Label {
    let label = gtk4::Label::new(Some(text));
    label.add_css_class(class);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    label
}

fn button(text: &str, tooltip: &str) -> gtk4::Button {
    let button = gtk4::Button::with_label(text);
    button.add_css_class("launcher-btn");
    button.set_tooltip_text(Some(tooltip));
    button
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

pub struct Browser {
    pub widget: gtk4::Box,
    search: gtk4::SearchEntry,
    category: gtk4::DropDown,
    order: gtk4::DropDown,
    status: gtk4::Label,
    results: gtk4::Box,
    entries: RefCell<Vec<Entry>>,
    categories: RefCell<Vec<String>>,
    loading: std::cell::Cell<bool>,
}

impl Browser {
    pub fn new() -> Rc<Self> {
        let widget = gtk4::Box::new(Orientation::Vertical, 8);
        widget.add_css_class("plugin-settings-row");
        widget.append(&label("Get plugins", "launcher-section-title"));
        widget.append(&label(
            "Plugins other people share, from the public list at github.com/gladimdim/super-desktop-plugins. \
             Listing is not a review: an installed plugin waits for your approval in the list below.",
            "launcher-note",
        ));
        let bar = gtk4::Box::new(Orientation::Horizontal, 6);
        let search = gtk4::SearchEntry::new();
        search.set_placeholder_text(Some("Search plugins"));
        search.set_hexpand(true);
        bar.append(&search);
        let category = gtk4::DropDown::from_strings(&[ALL]);
        category.set_tooltip_text(Some("Show one category"));
        bar.append(&category);
        let order = gtk4::DropDown::from_strings(&ORDERS);
        order.set_tooltip_text(Some("Group by category or list by name"));
        bar.append(&order);
        let refresh = button("↻", "Get the list again");
        bar.append(&refresh);
        widget.append(&bar);
        let status = label("", "launcher-note");
        widget.append(&status);
        let results = gtk4::Box::new(Orientation::Vertical, 10);
        results.add_css_class("harness-rows");
        widget.append(&results);

        let this = Rc::new(Self {
            widget,
            search,
            category,
            order,
            status,
            results,
            entries: RefCell::new(Vec::new()),
            categories: RefCell::new(Vec::new()),
            loading: std::cell::Cell::new(false),
        });
        let weak = Rc::downgrade(&this);
        this.search.connect_search_changed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.render();
            }
        });
        for dropdown in [&this.category, &this.order] {
            let weak = Rc::downgrade(&this);
            dropdown.connect_selected_notify(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.render();
                }
            });
        }
        let weak = Rc::downgrade(&this);
        refresh.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.fetch();
            }
        });
        // The last list at once; a fresh one when it arrives.
        if let Some((entries, when)) = registry::cached() {
            this.set_entries(entries);
            this.status.set_text(&format!("From the last check ({}); checking for changes…", age(when)));
        }
        this.fetch();
        this
    }

    fn set_entries(self: &Rc<Self>, entries: Vec<Entry>) {
        let categories = registry::categories(&entries);
        if *self.categories.borrow() != categories {
            let previous = self.selected_category();
            let mut names = vec![ALL.to_owned()];
            names.extend(categories.iter().cloned());
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            self.category.set_model(Some(&gtk4::StringList::new(&refs)));
            let keep = previous.and_then(|p| categories.iter().position(|c| *c == p)).map_or(0, |i| i + 1);
            *self.categories.borrow_mut() = categories;
            self.category.set_selected(keep as u32);
        }
        *self.entries.borrow_mut() = entries;
        self.render();
    }

    fn selected_category(&self) -> Option<String> {
        let index = self.category.selected() as usize;
        (index > 0).then(|| self.categories.borrow().get(index - 1).cloned()).flatten()
    }

    /// Gets the list again, off the GTK thread.
    pub fn fetch(self: &Rc<Self>) {
        if self.loading.replace(true) {
            return;
        }
        if self.entries.borrow().is_empty() {
            self.status.set_text("Getting the plugin list…");
        }
        let weak = Rc::downgrade(self);
        let handle = gio::spawn_blocking(registry::fetch);
        glib::spawn_future_local(async move {
            let result = handle.await.unwrap_or_else(|_| Err("the request failed".into()));
            let Some(this) = weak.upgrade() else { return };
            this.loading.set(false);
            match result {
                Ok((entries, skipped)) => {
                    let mut text = format!("{} plugins in the list.", entries.len());
                    if !skipped.is_empty() {
                        text.push_str(&format!(" Left out (not well formed): {}.", skipped.join(", ")));
                    }
                    this.status.remove_css_class("launcher-note-error");
                    this.status.set_text(&text);
                    this.set_entries(entries);
                }
                Err(e) if !this.entries.borrow().is_empty() => {
                    this.status.set_text(&format!("Could not check for changes ({e}); this is the list from the last check."));
                }
                Err(e) => {
                    this.status.add_css_class("launcher-note-error");
                    this.status.set_text(&e);
                }
            }
        });
    }

    /// Shows the entries that match the search and category, in order.
    pub fn render(self: &Rc<Self>) {
        while let Some(child) = self.results.first_child() {
            self.results.remove(&child);
        }
        let entries = self.entries.borrow().clone();
        if entries.is_empty() {
            return;
        }
        let category = self.selected_category();
        let order = if self.order.selected() == 1 { Order::Name } else { Order::Category };
        let found = registry::select(&entries, self.search.text().as_str(), category.as_deref(), order);
        if found.is_empty() {
            self.results.append(&label("No plugin matches.", "launcher-note"));
            return;
        }
        let mut group: Option<String> = None;
        for (entry, under) in found {
            if let Some(under) = under.filter(|u| group.as_ref() != Some(u)) {
                let title = label(&under, "launcher-section-title");
                title.add_css_class("plugin-browse-category");
                self.results.append(&title);
                group = Some(under);
            }
            self.results.append(&self.row(entry));
        }
    }

    fn row(self: &Rc<Self>, entry: &Entry) -> gtk4::Box {
        let card = gtk4::Box::new(Orientation::Vertical, 4);
        card.add_css_class("plugin-browse-row");
        let head = gtk4::Box::new(Orientation::Horizontal, 8);
        head.add_css_class("harness-row");
        let icon = gtk4::Label::new(Some("🧩"));
        icon.add_css_class("harness-icon");
        head.append(&icon);
        let name = gtk4::Label::new(Some(&entry.name));
        name.add_css_class("harness-name");
        head.append(&name);
        let detail = gtk4::Label::new(Some(&format!("by {} · {}", entry.author, entry.tags.join(", "))));
        detail.add_css_class("harness-cmd");
        detail.set_xalign(0.0);
        detail.set_hexpand(true);
        detail.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        head.append(&detail);
        card.append(&head);
        card.append(&label(&entry.description, "launcher-note"));
        if !entry.slots.is_empty() {
            card.append(&label(&format!("Adds: {} · {}", slots_text(&entry.slots), entry.web_url()), "harness-cmd"));
        }
        let warning = match entry.compatibility() {
            Compatibility::Ok => None,
            Compatibility::MadeForNewer(v) => Some(format!(
                "Made for SUPER DESKTOP {v} or newer; this is {}. It may not load until you update.",
                env!("CARGO_PKG_VERSION")
            )),
            Compatibility::NeedsApi(api) => Some(format!("Needs plugin API {api}: update SUPER DESKTOP to install it.")),
        };
        if let Some(warning) = &warning {
            let note = label(warning, "launcher-note");
            note.add_css_class("launcher-note-error");
            card.append(&note);
        }

        let actions = gtk4::Box::new(Orientation::Horizontal, 6);
        actions.add_css_class("launcher-actions");
        let note = label("", "launcher-note");
        note.set_visible(false);
        let places = Places::current();
        match here(&places, &entry.id) {
            Here::Installed => {
                let done = button("Installed", "This plugin is in your plugins folder");
                done.set_sensitive(false);
                actions.append(&done);
            }
            Here::WaitingApproval => {
                let done = button("Installed: approve it below", "Review and approve it in the list of your plugins");
                done.set_sensitive(false);
                actions.append(&done);
            }
            Here::No => {
                let get = button("Install", &format!("Get it from {}; it runs once you approve it", entry.web_url()));
                get.add_css_class("launcher-btn-primary");
                get.set_sensitive(!matches!(entry.compatibility(), Compatibility::NeedsApi(_)));
                let (url, id, note) = (entry.git_url(), entry.id.clone(), note.clone());
                get.connect_clicked(move |get| {
                    get.set_sensitive(false);
                    get.set_label("Installing…");
                    let (url, id, places) = (url.clone(), id.clone(), Places::current());
                    let handle = gio::spawn_blocking(move || install::install_as(&places, &url, Some(&id)).map(|i| i.manifest.name));
                    let (note, get) = (note.clone(), get.clone());
                    glib::spawn_future_local(async move {
                        let result = handle.await.unwrap_or_else(|_| Err("the install failed".into()));
                        match result {
                            Ok(name) => {
                                note.remove_css_class("launcher-note-error");
                                note.set_text(&format!("{name} is installed. It does not run yet: review and approve it in the list below."));
                                get.set_label("Installed: approve it below");
                            }
                            Err(e) => {
                                note.add_css_class("launcher-note-error");
                                note.set_text(&e);
                                get.set_label("Install");
                                get.set_sensitive(true);
                            }
                        }
                        note.set_visible(true);
                    });
                });
                actions.append(&get);
            }
        }
        let page = button("Open on GitHub", "Read its code and README in the browser");
        let url = entry.web_url();
        page.connect_clicked(move |_| {
            gtk4::UriLauncher::new(&url).launch(None::<&gtk4::Window>, gio::Cancellable::NONE, |_| {});
            // The browser opens under the overlay.
            if let Some(hooks) = super::api::hooks() {
                (hooks.hide)();
            }
        });
        actions.append(&page);
        actions.set_halign(Align::Start);
        card.append(&actions);
        card.append(&note);
        card
    }
}

/// "Top bar item, popup".
fn slots_text(slots: &[String]) -> String {
    slots.iter().map(|s| match s.as_str() {
        "toolbar.item" => "top bar item",
        "dock.left" => "left dock",
        "dock.right" => "right dock",
        "settings.page" => "settings section",
        "fullscreen" => "full screen",
        other => other,
    }).collect::<Vec<_>>().join(", ")
}

/// "5 minutes ago", "3 hours ago", "2 days ago".
fn age(when: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(when, |d| d.as_secs());
    let minutes = now.saturating_sub(when) / 60;
    match minutes {
        0 => "just now".into(),
        1..=59 => format!("{minutes} min ago"),
        60..=1439 => format!("{} h ago", minutes / 60),
        _ => format!("{} days ago", minutes / 1440),
    }
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
        let browser = Browser::new();
        let window = gtk4::Window::new();
        window.set_default_size(800, 700);
        window.set_child(Some(&browser.widget));
        window.present();

        let names = || crate::gtk_test::descendants(browser.results.upcast_ref()).into_iter()
            .filter_map(|w| w.downcast::<gtk4::Label>().ok())
            .filter(|l| l.has_css_class("harness-name"))
            .map(|l| l.text().to_string())
            .collect::<Vec<_>>();
        let headers = || crate::gtk_test::descendants(browser.results.upcast_ref()).into_iter()
            .filter_map(|w| w.downcast::<gtk4::Label>().ok())
            .filter(|l| l.has_css_class("plugin-browse-category"))
            .map(|l| l.text().to_string())
            .collect::<Vec<_>>();
        let buttons = |text: &str| crate::gtk_test::descendants(browser.results.upcast_ref()).into_iter()
            .filter_map(|w| w.downcast::<gtk4::Button>().ok())
            .filter(|b| b.label().as_deref() == Some(text))
            .collect::<Vec<_>>();

        // Grouped by category at first, the category list from the tags.
        crate::gtk_test::pump_until("the list", || names().len() == 3);
        assert_eq!(names(), ["Future", "Liar", "Clock"]);
        assert_eq!(headers(), ["games", "time"]);
        assert!(browser.status.text().contains("3 plugins"), "{}", browser.status.text());
        // By name; a search; a category.
        browser.order.set_selected(1);
        assert_eq!(names(), ["Clock", "Future", "Liar"]);
        assert!(headers().is_empty());
        browser.search.set_text("dock");
        crate::gtk_test::pump_until("the search", || names() == ["Clock"]);
        browser.search.set_text("");
        crate::gtk_test::pump_until("the search cleared", || names().len() == 3);
        browser.category.set_selected(1); // games
        assert_eq!(names(), ["Future", "Liar"]);
        browser.category.set_selected(0);

        // A newer API cannot be installed; Install clones and waits for approval.
        assert_eq!(buttons("Install").iter().filter(|b| b.is_sensitive()).count(), 2);
        let install_clock = buttons("Install").into_iter().find(|b| b.tooltip_text().is_some_and(|t| t.contains("me/clock"))).unwrap();
        install_clock.emit_clicked();
        crate::gtk_test::pump_until("the install", || install_clock.label().as_deref() == Some("Installed: approve it below"));
        let places = Places::current();
        assert!(places.plugins.join("clock/plugin.json").is_file());
        assert_eq!(here(&places, "clock"), Here::WaitingApproval);
        // An entry whose repository holds another id is refused, nothing installed.
        let install_liar = buttons("Install").into_iter().find(|b| b.tooltip_text().is_some_and(|t| t.contains("me/liar"))).unwrap();
        install_liar.emit_clicked();
        crate::gtk_test::pump_until("the refusal", || install_liar.is_sensitive());
        let refusal = crate::gtk_test::descendants(browser.results.upcast_ref()).into_iter()
            .filter_map(|w| w.downcast::<gtk4::Label>().ok())
            .find(|l| l.is_visible() && l.text().contains("not \"liar\""));
        assert!(refusal.is_some(), "the refusal says why");
        assert!(!places.plugins.join("something-else").exists() && !places.plugins.join("liar").exists());
        // Shown again, it says installed.
        browser.render();
        assert_eq!(buttons("Installed: approve it below").len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
