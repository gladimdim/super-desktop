//! Plugin buttons in the header of terminal cards (`cardButtons` in
//! plugin.json). They sit before the card's own buttons and only take room
//! the header has to spare: the header sets the card's minimum width. With
//! too little room for all of them they fold into one ⋯ menu, and on the
//! narrowest cards they are not shown. A click reaches every running surface
//! of the plugin as `sd.cardButtonClicked(button, card)`.
use super_desktop::plugins::{manifest::CardButton, protocol};
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::ObjectSubclassIsExt;

/// One button of one running plugin.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub plugin: String,
    pub plugin_name: String,
    pub button: CardButton,
}

/// The card buttons of the plugins running now.
pub fn current() -> Vec<Spec> {
    super::slots::get().map(|slots| slots.card_buttons()).unwrap_or_default()
}

/// Space between the buttons of one row.
const SPACING: i32 = 2;

/// How a row shows its buttons in the room it got.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Shape {
    #[default]
    Inline,
    /// One ⋯ button with a menu of them.
    Folded,
    /// Not even ⋯ fits.
    Hidden,
}

/// Tells the plugin that `button` was clicked on the card `card_id`.
/// Returns how many of its surfaces got it.
pub fn click(plugin: &str, button: &str, card_id: &str) -> usize {
    let card = super::api::hooks()
        .and_then(|hooks| (hooks.cards)().as_array()?.iter().find(|c| c["id"] == card_id).cloned());
    let Some(card) = card else { return 0 };
    let sent = super::surface::broadcast(plugin, &protocol::card_button(button, &card));
    super_desktop::plugins::paths::append_log(plugin, &format!("card button {button} on {card_id}: told {sent} surface(s)"));
    sent
}

mod imp {
    use gtk4::glib;
    use gtk4::prelude::*;
    use gtk4::subclass::prelude::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct ButtonRow {
        pub card: RefCell<String>,
        pub shell: Cell<bool>,
        pub shown: RefCell<Vec<super::Spec>>,
        pub buttons: RefCell<Vec<gtk4::Button>>,
        pub more: RefCell<Option<gtk4::Button>>,
        /// How the last allocation showed them.
        pub shape: Cell<super::Shape>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ButtonRow {
        const NAME: &'static str = "SdPluginCardButtons";
        type Type = super::ButtonRow;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for ButtonRow {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl ButtonRow {
        /// The inline buttons' natural width side by side.
        pub fn inline_width(&self) -> i32 {
            let buttons = self.buttons.borrow();
            let widths: i32 = buttons.iter().map(|b| b.measure(gtk4::Orientation::Horizontal, -1).1).sum();
            widths + super::SPACING * (buttons.len() as i32 - 1).max(0)
        }

        fn more_width(&self) -> i32 {
            self.more.borrow().as_ref().map_or(0, |more| more.measure(gtk4::Orientation::Horizontal, -1).1)
        }
    }

    impl WidgetImpl for ButtonRow {
        fn measure(&self, orientation: gtk4::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if self.buttons.borrow().is_empty() {
                return (0, 0, -1, -1);
            }
            match orientation {
                // Any room the header can spare, up to all of them inline.
                gtk4::Orientation::Horizontal => (0, self.inline_width(), -1, -1),
                _ => {
                    let mut children: Vec<gtk4::Widget> = self.buttons.borrow().iter().map(|b| b.clone().upcast()).collect();
                    children.extend(self.more.borrow().iter().map(|m| m.clone().upcast()));
                    let (min, natural) = children.iter()
                        .map(|c| c.measure(gtk4::Orientation::Vertical, for_size))
                        .fold((0, 0), |(min, natural), (m, n, _, _)| (min.max(m), natural.max(n)));
                    (min, natural, -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let shape = if width >= self.inline_width() {
                super::Shape::Inline
            } else if width >= self.more_width() {
                super::Shape::Folded
            } else {
                super::Shape::Hidden
            };
            self.shape.set(shape);
            let at = |x: i32| Some(gtk4::gsk::Transform::new().translate(&gtk4::graphene::Point::new(x as f32, 0.0)));
            let mut x = 0;
            for button in self.buttons.borrow().iter() {
                button.set_child_visible(shape == super::Shape::Inline);
                let w = button.measure(gtk4::Orientation::Horizontal, -1).1;
                if shape == super::Shape::Inline {
                    button.allocate(w, height, -1, at(x));
                }
                x += w + super::SPACING;
            }
            if let Some(more) = self.more.borrow().as_ref() {
                more.set_child_visible(shape == super::Shape::Folded);
                if shape == super::Shape::Folded {
                    let w = self.more_width();
                    more.allocate(w, height, -1, at(width - w));
                }
            }
        }
    }
}

glib::wrapper! {
    /// The plugin buttons of one terminal card's header.
    pub struct ButtonRow(ObjectSubclass<imp::ButtonRow>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl ButtonRow {
    /// The row of the card `card_id`; `shell` says whether it runs a plain
    /// shell rather than an agent (for buttons limited to one kind).
    pub fn new(card_id: &str, shell: bool) -> Self {
        let row: Self = glib::Object::new();
        row.add_css_class("plugin-card-btns");
        *row.imp().card.borrow_mut() = card_id.to_owned();
        row.imp().shell.set(shell);
        row.set_visible(false);
        row.set(&current());
        row
    }

    /// Shows `specs` that belong on this card, unless they already are.
    pub fn set(&self, specs: &[Spec]) {
        let imp = self.imp();
        let wanted: Vec<Spec> = specs.iter().filter(|s| s.button.shows_on(imp.shell.get())).cloned().collect();
        if *imp.shown.borrow() == wanted {
            return;
        }
        for button in imp.buttons.take() {
            button.unparent();
        }
        if let Some(more) = imp.more.take() {
            more.unparent();
        }
        let card = imp.card.borrow().clone();
        let mut buttons = Vec::new();
        for spec in &wanted {
            let button = gtk4::Button::with_label(&spec.button.icon);
            button.add_css_class("term-btn");
            button.add_css_class("plugin-card-btn");
            button.set_tooltip_text(Some(&format!("{} · {}", spec.button.tooltip, spec.plugin_name)));
            button.set_focus_on_click(false);
            let (plugin, id, card) = (spec.plugin.clone(), spec.button.id.clone(), card.clone());
            button.connect_clicked(move |_| {
                click(&plugin, &id, &card);
            });
            button.set_parent(self);
            buttons.push(button);
        }
        let more = (!wanted.is_empty()).then(|| {
            let menu = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
            menu.add_css_class("plugin-card-menu");
            let popover = gtk4::Popover::new();
            popover.add_css_class("ws-pop");
            popover.set_child(Some(&menu));
            for spec in &wanted {
                let item = gtk4::Button::with_label(&format!("{}  {}", spec.button.icon, spec.button.tooltip));
                item.add_css_class("flat");
                item.set_tooltip_text(Some(&spec.plugin_name));
                if let Some(label) = item.child().and_downcast::<gtk4::Label>() {
                    label.set_xalign(0.0);
                }
                let (plugin, id, card) = (spec.plugin.clone(), spec.button.id.clone(), card.clone());
                let weak = popover.downgrade();
                item.connect_clicked(move |_| {
                    if let Some(popover) = weak.upgrade() {
                        popover.popdown();
                    }
                    click(&plugin, &id, &card);
                });
                menu.append(&item);
            }
            // A plain button styled like its neighbours (a MenuButton's inner
            // button keeps GTK's wider padding).
            let more = gtk4::Button::with_label("⋯");
            more.add_css_class("term-btn");
            more.add_css_class("plugin-card-more");
            more.set_tooltip_text(Some("Plugin actions"));
            more.set_focus_on_click(false);
            popover.set_parent(&more);
            more.connect_clicked(move |_| popover.popup());
            more.set_child_visible(false);
            more.set_parent(self);
            more
        });
        *imp.buttons.borrow_mut() = buttons;
        *imp.more.borrow_mut() = more;
        *imp.shown.borrow_mut() = wanted;
        self.set_visible(!imp.shown.borrow().is_empty());
        self.queue_resize();
    }

    /// The buttons, the ⋯ button, and how they are shown now.
    #[cfg(test)]
    pub fn state(&self) -> (Vec<gtk4::Button>, Option<gtk4::Button>, Shape) {
        let imp = self.imp();
        (imp.buttons.borrow().clone(), imp.more.borrow().clone(), imp.shape.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(plugin: &str, id: &str, icon: &str, cards: &str) -> Spec {
        Spec {
            plugin: plugin.into(),
            plugin_name: plugin.to_uppercase(),
            button: CardButton { id: id.into(), icon: icon.into(), tooltip: format!("Do {id}"), cards: cards.into() },
        }
    }

    #[test]
    fn plugins_card_buttons_fold_and_fit_the_header() {
        crate::gtk_test::run_in_child_process("plugin_ui::card_buttons::tests::fold_and_fit_inner");
    }

    /// A card shows the buttons meant for its kind, in a row that folds into
    /// ⋯ instead of widening the header, which sets the card's minimum width.
    #[test]
    fn fold_and_fit_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let specs = vec![
            spec("tiles", "left", "◧", "all"),
            spec("tiles", "right", "◨", "all"),
            spec("helper", "sum", "✦", "agents"),
            spec("helper", "shell", "$", "shells"),
        ];
        let data = |id: &str, agent: &str| crate::state::TerminalData {
            id: id.into(), session_name: id.into(), agent_type: agent.into(), command: agent.into(),
            x: 0, y: 0, width: 380, height: 300, restored_width: 380, restored_height: 300,
            iconified: false, icon_x: None, icon_y: None, created_at: 0.0, tag: 0,
            agent_session_id: None, workspace_dir: Some("/tmp".into()),
        };
        let agent = crate::mini_terminal::MiniTerminalCard::for_test(data("sd_term_agent", "claude"), |_, _| {});
        let shell = crate::mini_terminal::MiniTerminalCard::for_test(data("sd_term_shell", "shell"), |_, _| {});
        let row = agent.plugin_buttons().expect("a local card has a plugin row").clone();
        assert!(!row.is_visible(), "no plugins, no row");
        agent.set_plugin_buttons(&specs);
        shell.set_plugin_buttons(&specs);
        let icons = |row: &ButtonRow| row.state().0.iter().map(|b| b.label().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(icons(&row), ["◧", "◨", "✦"]);
        assert_eq!(icons(shell.plugin_buttons().unwrap()), ["◧", "◨", "$"]);
        assert!(row.is_visible());
        assert_eq!(row.state().0[0].tooltip_text().unwrap(), "Do left · TILES");
        // The same buttons again change nothing.
        let first = row.state().0[0].clone();
        agent.set_plugin_buttons(&specs);
        assert_eq!(row.state().0[0], first);

        // The buttons never widen the card: the header needs what it needed
        // without them, and still fits a default-size card.
        let (min, natural, _, _) = row.measure(gtk4::Orientation::Horizontal, -1);
        assert_eq!(min, 0);
        assert!(natural > 40, "three buttons side by side: {natural}px");
        let root = agent.container.clone().upcast::<gtk4::Widget>();
        let header = crate::gtk_test::find_all::<gtk4::Widget>(&root, "term-header").pop().unwrap();
        let badge = crate::gtk_test::find_all::<gtk4::Label>(&header, "term-status-badge").pop().unwrap();
        for status in ["● IDLE", "○ EXITED", "● WORKING"] {
            badge.set_label(status);
            let width = header.measure(gtk4::Orientation::Horizontal, -1).0;
            assert!(width + 4 <= crate::mini_terminal::CARD_WIDTH, "{status}: the header needs {width}px");
        }

        // Room for all: inline. Less: one ⋯ that lists them all. Less than
        // that: nothing.
        let row = ButtonRow::new("sd_term_x", false);
        row.set(&specs);
        let title = gtk4::Label::new(Some(&"A long prompt the user typed ".repeat(4)));
        title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        title.set_hexpand(true);
        // Takes the room of the header's other parts.
        let others = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        line.append(&others);
        line.append(&title);
        line.append(&row);
        let window = gtk4::Window::new();
        window.set_child(Some(&line));
        window.set_default_size(300, 40);
        window.present();
        let settle = || {
            for _ in 0..30 {
                while gtk4::glib::MainContext::default().iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        };
        let natural = row.measure(gtk4::Orientation::Horizontal, -1).1;
        let more_width = row.state().1.unwrap().measure(gtk4::Orientation::Horizontal, -1).1;
        let mut seen = Vec::new();
        for taken in [0, 200, 230, 250, 270, 290] {
            others.set_size_request(taken, -1);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while others.width() != taken && std::time::Instant::now() < deadline {
                settle();
            }
            settle();
            let (buttons, more, shape) = row.state();
            let width = row.width();
            let expected = if width >= natural { Shape::Inline } else if width >= more_width { Shape::Folded } else { Shape::Hidden };
            assert_eq!(shape, expected, "{width}px of {natural} (⋯ {more_width})");
            assert!(buttons.iter().all(|b| b.is_child_visible() == (shape == Shape::Inline)));
            assert_eq!(more.unwrap().is_child_visible(), shape == Shape::Folded);
            if !seen.contains(&shape) {
                seen.push(shape);
            }
        }
        assert_eq!(seen, [Shape::Inline, Shape::Folded, Shape::Hidden], "every shape, widest first");
        let more = row.state().1.unwrap();
        let popover = std::iter::successors(more.first_child(), |w| w.next_sibling())
            .find_map(|w| w.downcast::<gtk4::Popover>().ok())
            .expect("⋯ has its menu");
        let menu = popover.child().unwrap();
        let items: Vec<String> = std::iter::successors(menu.first_child(), |w| w.next_sibling())
            .map(|w| w.downcast::<gtk4::Button>().unwrap().label().unwrap().to_string())
            .collect();
        assert_eq!(items, ["◧  Do left", "◨  Do right", "✦  Do sum"]);
        // No plugins any more: the row goes away.
        row.set(&[]);
        assert!(!row.is_visible());
        assert!(row.state().0.is_empty() && row.state().1.is_none());
        window.close();
    }
}
