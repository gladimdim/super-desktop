//! Dotted "there is a terminal under here" outlines for buried cards.
//!
//! Two terminals of the overlay can end up stacked: a card dropped on another
//! one, a freshly launched harness cascading 32px over the previous one, an
//! auto-arrange that stacks a column. The top card hides the one underneath, so
//! while the user is working in neither of them the buried card's rectangle is
//! drawn as a dotted ghost outline — the desk still shows that a terminal is
//! there.
//!
//! Only terminals count as coverers (a sticky note is decoration), only cards
//! painted *above* the buried one hide it (a raised card is fully visible), and
//! nothing is drawn over the card the user is working in.
//!
//! The planner ([`ghost_indexes`], [`covered_ratio`]) is plain geometry that
//! unit tests cover; [`GhostLayer`] is the thin GTK side that places one
//! outline widget per hidden card.

use gtk4::prelude::*;
use gtk4::{Fixed, Widget};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::card_resize::Rect;
use crate::mini_terminal::MiniTerminalCard;

/// Fraction of a card that must be covered, by the cards painted above it, for
/// that card to earn a ghost outline.
pub const OVERLAP_HIDE_RATIO: f64 = 0.70;

/// One terminal as the ghost planner sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalPlacement {
    /// tmux session name: identifies the outline that belongs to this card.
    pub session: String,
    /// Where the card is drawn on the overlay canvas.
    pub rect: Rect,
    /// The user is working in this card right now (see
    /// `MiniTerminalCard::user_is_active`).
    pub user_active: bool,
}

/// Indexes into `placements` (bottom-most first) that are hidden enough to earn
/// a ghost outline.
///
/// A card is hidden only by the cards painted above it, and only while neither
/// it nor any of those coverers is the card the user is working in: a dotted
/// outline over the terminal somebody is typing in is noise.
pub fn ghost_indexes(placements: &[TerminalPlacement]) -> Vec<usize> {
    let mut ghosts = Vec::new();
    for (index, card) in placements.iter().enumerate() {
        let above = &placements[index + 1..];
        if card.user_active || above.iter().any(|coverer| coverer.user_active) {
            continue;
        }
        let coverers: Vec<Rect> = above.iter().map(|coverer| coverer.rect).collect();
        if covered_ratio(card.rect, &coverers) >= OVERLAP_HIDE_RATIO {
            ghosts.push(index);
        }
    }
    ghosts
}

/// Fraction of `target` covered by the union of `coverers`, `0.0`–`1.0`.
///
/// The union, not a sum: two cards that each cover half of a third one do hide
/// it together, while coverers that overlap each other must not count the same
/// pixels twice.
pub fn covered_ratio(target: Rect, coverers: &[Rect]) -> f64 {
    let area = target.width as f64 * target.height as f64;
    if area <= 0.0 {
        return 0.0;
    }
    let hidden: Vec<Area> = coverers
        .iter()
        .filter_map(|coverer| Area::intersection(target, *coverer))
        .collect();
    (Area::union(&hidden) / area).clamp(0.0, 1.0)
}

/// Axis-aligned rectangle with floating-point edges, so an intersection never
/// loses area to rounding.
#[derive(Clone, Copy)]
struct Area {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Area {
    fn intersection(a: Rect, b: Rect) -> Option<Self> {
        let area = Self {
            x0: a.x.max(b.x),
            y0: a.y.max(b.y),
            x1: (a.x + a.width as f64).min(b.x + b.width as f64),
            y1: (a.y + a.height as f64).min(b.y + b.height as f64),
        };
        (area.x1 > area.x0 && area.y1 > area.y0).then_some(area)
    }

    /// Area of the union of `rects`, by splitting them on every vertical edge
    /// and merging the Y spans inside each slab.
    fn union(rects: &[Area]) -> f64 {
        let mut edges: Vec<f64> = rects
            .iter()
            .flat_map(|rect| [rect.x0, rect.x1])
            .collect();
        edges.sort_by(f64::total_cmp);
        edges.dedup();

        let mut total = 0.0;
        for slab in edges.windows(2) {
            let (left, right) = (slab[0], slab[1]);
            let spans = rects
                .iter()
                .filter(|rect| rect.x0 <= left && rect.x1 >= right)
                .map(|rect| (rect.y0, rect.y1));
            total += (right - left) * merged_height(spans);
        }
        total
    }
}

/// Total length of the merged Y intervals in `spans`.
fn merged_height(spans: impl Iterator<Item = (f64, f64)>) -> f64 {
    let mut spans: Vec<(f64, f64)> = spans.collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut height = 0.0;
    let mut open: Option<(f64, f64)> = None;
    for (top, bottom) in spans {
        open = match open {
            Some((start, end)) if top <= end => Some((start, end.max(bottom))),
            Some((start, end)) => {
                height += end - start;
                Some((top, bottom))
            }
            None => Some((top, bottom)),
        };
    }
    if let Some((start, end)) = open {
        height += end - start;
    }
    height
}

/// The dotted outlines drawn over buried cards.
///
/// One outline widget per card, created on demand and kept in the canvas, so a
/// refresh that changes nothing moves, restyles and restacks nothing: the same
/// call runs from pointer and focus callbacks, from drag frames and from the
/// periodic refresh.
pub struct GhostLayer {
    canvas: Fixed,
    hud: gtk4::Box,
    cards: Rc<RefCell<Vec<Rc<MiniTerminalCard>>>>,
    screen_w: i32,
    screen_h: i32,
    /// False while the overlay slides in or out, or is unmapped: a ghost must
    /// not hang in mid-air at rest poses the cards have not reached yet.
    live: Cell<bool>,
    outlines: RefCell<Vec<Outline>>,
}

/// One card's outline widget and the geometry currently applied to it.
struct Outline {
    session: String,
    widget: gtk4::Box,
    rect: Cell<Option<Rect>>,
}

impl GhostLayer {
    pub fn new(
        canvas: &Fixed,
        hud: &gtk4::Box,
        cards: Rc<RefCell<Vec<Rc<MiniTerminalCard>>>>,
        screen_w: i32,
        screen_h: i32,
    ) -> Rc<Self> {
        Rc::new(Self {
            canvas: canvas.clone(),
            hud: hud.clone(),
            cards,
            screen_w,
            screen_h,
            live: Cell::new(true),
            outlines: RefCell::new(Vec::new()),
        })
    }

    /// Recompute the outlines from the live cards and their stacking order.
    pub fn refresh(&self) {
        if !self.live.get() {
            return;
        }
        let cards = self.cards.borrow().clone();
        let placements: Vec<TerminalPlacement> = paint_order(&self.canvas, &cards)
            .iter()
            .map(|card| TerminalPlacement {
                session: card.data.borrow().session_name.clone(),
                rect: card.canvas_rect(self.screen_w, self.screen_h),
                user_active: card.user_is_active(),
            })
            .collect();
        self.apply(&placements);
    }

    /// Draw an outline for every buried card in `placements` and hide the rest.
    pub fn apply(&self, placements: &[TerminalPlacement]) {
        let buried: Vec<(String, Rect)> = ghost_indexes(placements)
            .iter()
            .map(|&index| {
                let card = &placements[index];
                (card.session.clone(), card.rect)
            })
            .collect();

        let mut visible: Vec<Widget> = Vec::with_capacity(buried.len());
        {
            let mut outlines = self.outlines.borrow_mut();
            for (session, rect) in buried.iter() {
                match outlines
                    .iter()
                    .position(|outline| &outline.session == session)
                {
                    Some(index) => outlines[index].show(*rect),
                    None => {
                        let outline = Outline::new(session, &self.canvas, *rect);
                        outline.show(*rect);
                        outlines.push(outline);
                    }
                }
            }
            for outline in outlines.iter() {
                if !buried.iter().any(|(session, _)| session == &outline.session) {
                    outline.hide();
                }
            }
            visible.extend(
                outlines
                    .iter()
                    .filter(|outline| outline.widget.is_visible())
                    .map(|outline| outline.widget.clone().upcast()),
            );
        }

        self.restack(&visible);
        self.prune(placements);
    }

    /// Take every outline off screen and stop reacting to card changes.
    pub fn suspend(&self) {
        self.live.set(false);
        for outline in self.outlines.borrow().iter() {
            outline.hide();
        }
    }

    /// Allow outlines again (the slide settled) and redraw them.
    pub fn resume(&self) {
        self.live.set(true);
        self.refresh();
    }

    /// Keep the outlines above the cards they hide, but under the toolbar.
    ///
    /// Read-only unless a card was raised above them (hover, drag start,
    /// expand) or a fresh outline was appended on top of the canvas: restacking
    /// on every refresh would relayout the canvas on every frame of a drag.
    fn restack(&self, visible: &[Widget]) {
        if visible.is_empty() {
            return;
        }
        if self.buried(visible) {
            for outline in visible {
                if let Some(last) = self.canvas.last_child() {
                    if &last != outline {
                        outline.insert_after(&self.canvas, Some(&last));
                    }
                }
            }
        }
        // A new outline is `put` at the top of the canvas, which would leave it
        // painted over the toolbar until the next raise.
        if self.hud.next_sibling().is_some() {
            crate::window::raise_canvas_child(&self.canvas, &self.hud);
        }
    }

    /// True when a card (or anything else but the toolbar) sits above the
    /// outlines, i.e. they have to be restacked to be seen at all.
    fn buried(&self, visible: &[Widget]) -> bool {
        let hud = self.hud.clone().upcast::<Widget>();
        let mut child = self.canvas.last_child();
        while let Some(widget) = child {
            if widget == hud {
                child = widget.prev_sibling();
                continue;
            }
            return !visible.iter().any(|outline| outline == &widget);
        }
        false
    }

    /// Forget the outlines of cards that are gone: their widget is unparented,
    /// so the canvas does not accumulate children for closed terminals.
    fn prune(&self, placements: &[TerminalPlacement]) {
        let lost: Vec<Outline> = {
            let mut outlines = self.outlines.borrow_mut();
            let mut lost = Vec::new();
            let mut index = 0;
            while index < outlines.len() {
                if placements
                    .iter()
                    .any(|card| card.session == outlines[index].session)
                {
                    index += 1;
                } else {
                    lost.push(outlines.remove(index));
                }
            }
            lost
        };
        // Unparent outside the borrow: `remove` emits pointer/focus signals
        // that can call back into a refresh (see the note on
        // `SuperDesktopWindow::terminal_cards`).
        for outline in lost {
            self.canvas.remove(&outline.widget);
        }
    }
}

impl Outline {
    fn new(session: &str, canvas: &Fixed, rect: Rect) -> Self {
        let widget = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        widget.add_css_class("term-overlap-ghost");
        // A hint, never a target: clicking "through" the outline still lands on
        // the terminal it covers.
        widget.set_can_target(false);
        canvas.put(&widget, rect.x, rect.y);

        let outline = Self {
            session: session.to_string(),
            widget,
            rect: Cell::new(None),
        };
        outline.place(rect);
        outline
    }

    fn show(&self, rect: Rect) {
        self.place(rect);
        if !self.widget.is_visible() {
            self.widget.set_visible(true);
        }
    }

    fn hide(&self) {
        // Forget the geometry so the next show re-places the outline: the card
        // may have been dragged, arranged or resized meanwhile.
        self.rect.set(None);
        if self.widget.is_visible() {
            self.widget.set_visible(false);
        }
    }

    fn place(&self, rect: Rect) {
        if self.rect.get() == Some(rect) {
            return;
        }
        if let Some(parent) = self.widget.parent().and_then(|p| p.downcast::<Fixed>().ok()) {
            parent.move_(&self.widget, rect.x, rect.y);
        }
        self.widget.set_size_request(rect.width, rect.height);
        self.rect.set(Some(rect));
    }
}

/// Cards in canvas paint order, bottom-most first.
///
/// The card list is raise order, not stacking order: `expand` re-inserts a card
/// at the top of the canvas without touching the list, and a card being
/// reparented is missing from the canvas for a frame.
fn paint_order(canvas: &Fixed, cards: &[Rc<MiniTerminalCard>]) -> Vec<Rc<MiniTerminalCard>> {
    let mut ordered: Vec<Rc<MiniTerminalCard>> = Vec::with_capacity(cards.len());
    let mut child = canvas.first_child();
    while let Some(widget) = child {
        if let Some(card) = cards
            .iter()
            .find(|card| card.container.upcast_ref::<Widget>() == &widget)
        {
            ordered.push(Rc::clone(card));
        }
        child = widget.next_sibling();
    }
    for card in cards {
        if !ordered.iter().any(|placed| placed.container == card.container) {
            ordered.push(Rc::clone(card));
        }
    }
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect {
            x: x as f64,
            y: y as f64,
            width,
            height,
        }
    }

    fn card(session: &str, rect: Rect, user_active: bool) -> TerminalPlacement {
        TerminalPlacement {
            session: session.to_string(),
            rect,
            user_active,
        }
    }

    /// The sessions that earn an outline, in placement order.
    fn ghosted(placements: &[TerminalPlacement]) -> Vec<String> {
        ghost_indexes(placements)
            .into_iter()
            .map(|index| placements[index].session.clone())
            .collect()
    }

    #[test]
    fn covered_ratio_uses_the_union_of_the_coverers() {
        let target = rect(0, 0, 100, 100);
        assert_eq!(covered_ratio(target, &[]), 0.0);
        assert_eq!(covered_ratio(target, &[rect(200, 200, 50, 50)]), 0.0);
        assert_eq!(covered_ratio(target, &[rect(0, 0, 100, 100)]), 1.0);
        assert_eq!(covered_ratio(target, &[rect(0, 0, 50, 100)]), 0.5);
        // Two coverers over the same half count it once, not twice.
        assert_eq!(
            covered_ratio(target, &[rect(0, 0, 50, 100), rect(0, 0, 50, 100)]),
            0.5
        );
        // Partly overlapping coverers only count the shared pixels once.
        assert_eq!(
            covered_ratio(target, &[rect(0, 0, 50, 100), rect(0, 0, 60, 50)]),
            0.55
        );
        // Disjoint coverers add up.
        assert_eq!(
            covered_ratio(target, &[rect(0, 0, 50, 100), rect(50, 0, 50, 100)]),
            1.0
        );
        // Hanging off the edge of the target only counts what is inside it.
        assert_eq!(covered_ratio(target, &[rect(-50, -50, 100, 100)]), 0.25);
        assert_eq!(covered_ratio(rect(0, 0, 0, 0), &[rect(0, 0, 10, 10)]), 0.0);
    }

    #[test]
    fn a_card_hidden_by_seventy_percent_earns_an_outline() {
        let under = card("under", rect(100, 100, 400, 300), false);
        // 72% of the card's width covered: 288 of 400 pixels.
        let almost = card("over", rect(100, 100, 288, 300), false);
        assert_eq!(ghosted(&[under.clone(), almost]), ["under"]);

        // 69%: not enough.
        let just_under = card("over", rect(100, 100, 276, 300), false);
        assert!(ghost_indexes(&[under, just_under]).is_empty());
    }

    #[test]
    fn only_cards_painted_above_hide_a_card() {
        // The small card sits on the big one and hides 88% of it.
        let big = card("big", rect(0, 0, 400, 300), false);
        let small = card("small", rect(20, 20, 380, 280), false);

        // `small` is painted above `big`: the big card is the buried one.
        assert_eq!(ghosted(&[big.clone(), small.clone()]), ["big"]);
        // Painted the other way round the same pair buries the small card.
        assert_eq!(ghosted(&[small, big]), ["small"]);
    }

    #[test]
    fn coverers_are_merged_across_several_cards() {
        let under = card("under", rect(0, 0, 400, 300), false);
        // Two cards that each hide 40% of `under` from different sides: the
        // pair of them covers 80%, so the card is buried.
        let left = card("left", rect(0, 0, 160, 300), false);
        let right = card("right", rect(240, 0, 160, 300), false);
        assert_eq!(ghosted(&[under, left, right]), ["under"]);
    }

    #[test]
    fn nothing_is_drawn_over_the_card_the_user_is_working_in() {
        let under = card("under", rect(0, 0, 400, 300), false);
        let over = card("over", rect(20, 0, 380, 300), false);

        // The buried card is the one in use: leave it alone.
        let mut active_under = under.clone();
        active_under.user_active = true;
        assert!(ghost_indexes(&[active_under, over.clone()]).is_empty());

        // The card doing the hiding is in use: no dotted outline over it.
        let mut active_over = over.clone();
        active_over.user_active = true;
        assert!(ghost_indexes(&[under.clone(), active_over]).is_empty());

        // Neither is in use: the outline is back.
        assert_eq!(ghosted(&[under, over]), ["under"]);
    }

    #[test]
    fn an_icon_can_be_hidden_too() {
        // An iconified card is a 128×128 square: a card sitting on it hides it.
        let icon = card("icon", rect(600, 400, 128, 128), false);
        let over = card("over", rect(560, 360, 400, 300), false);
        assert_eq!(ghosted(&[icon, over]), ["icon"]);
    }

    /// Let GTK run an allocation pass: `Fixed::child_position` only reports the
    /// new spot once the canvas has laid out again.
    fn pump() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            while gtk4::glib::MainContext::default().iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Widgets the layer keeps on the canvas for the buried cards.
    fn outlines(canvas: &Fixed) -> Vec<Widget> {
        let mut found = Vec::new();
        let mut child = canvas.first_child();
        while let Some(widget) = child {
            if widget.has_css_class("term-overlap-ghost") {
                found.push(widget.clone());
            }
            child = widget.next_sibling();
        }
        found
    }

    #[test]
    fn outlines_follow_the_buried_cards_on_the_canvas() {
        crate::gtk_test::run_in_child_process("overlap_ghost::tests::ghost_layer_gtk");
    }

    #[test]
    fn ghost_layer_gtk() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        let app = gtk4::Application::new(
            Some("com.superdesktop.GhostTest"),
            gtk4::gio::ApplicationFlags::NON_UNIQUE,
        );
        app.register(None::<&gtk4::gio::Cancellable>).unwrap();
        let window = gtk4::ApplicationWindow::new(&app);
        let canvas = Fixed::new();
        let hud = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        let hud_widget = hud.clone().upcast::<Widget>();
        canvas.put(&hud, 0.0, 0.0);
        // Stands in for the card that is painted below the outlines.
        let coverer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        canvas.put(&coverer, 0.0, 0.0);
        window.set_child(Some(&canvas));
        window.present();

        // Cards live in the window's own list; the layer only needs the canvas,
        // the toolbar and the screen size here.
        let layer = GhostLayer::new(
            &canvas,
            &hud,
            Rc::new(RefCell::new(Vec::<Rc<MiniTerminalCard>>::new())),
            1920,
            1080,
        );
        let buried = TerminalPlacement {
            session: "buried".to_string(),
            rect: rect(10, 20, 400, 300),
            user_active: false,
        };
        let coverer_card = TerminalPlacement {
            session: "coverer".to_string(),
            rect: rect(30, 20, 380, 300),
            user_active: false,
        };

        layer.apply(&[buried.clone(), coverer_card.clone()]);
        let found = outlines(&canvas);
        assert_eq!(found.len(), 1, "one outline for the one buried card");
        let outline = &found[0];
        assert!(outline.is_visible(), "the outline must be on screen");
        // A canvas move only lands on the next allocation (the same reason the
        // slide tick paints a frame after `move_`), so let one pass run.
        pump();
        assert_eq!(canvas.child_position(outline), (10.0, 20.0));
        assert_eq!(outline.size_request(), (400, 300));
        // Drawn over the card that hides it, under the toolbar.
        assert_eq!(coverer.next_sibling().as_ref(), Some(outline));
        assert_eq!(outline.next_sibling().as_ref(), Some(&hud_widget));

        // Working in the card that does the hiding takes the outline away…
        let mut active_coverer = coverer_card.clone();
        active_coverer.user_active = true;
        layer.apply(&[buried.clone(), active_coverer]);
        assert!(!outlines(&canvas)[0].is_visible());

        // …and the same outline comes back once nobody is in either card.
        layer.apply(&[buried.clone(), coverer_card]);
        assert!(outlines(&canvas)[0].is_visible());

        // A slide parks the desks: no outlines in mid-air.
        layer.suspend();
        assert!(outlines(&canvas).iter().all(|outline| !outline.is_visible()));

        // A closed card takes its outline (and only its outline) with it.
        layer.resume();
        layer.apply(&[]);
        assert!(outlines(&canvas).is_empty());
        assert!(hud.parent().is_some(), "the toolbar stays in the canvas");
    }
}

#[cfg(test)]
mod preview_probe {
    use super::*;
    use gtk4::prelude::*;

    #[test]
    fn render_preview() {
        crate::gtk_test::run_in_child_process("overlap_ghost::preview_probe::render_preview_inner");
    }

    #[test]
    fn render_preview_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let app = gtk4::Application::new(Some("com.superdesktop.GhostPreview"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
        app.register(None::<&gtk4::gio::Cancellable>).unwrap();
        let window = gtk4::ApplicationWindow::new(&app);
        window.set_default_size(900, 600);
        let canvas = Fixed::new();
        let backdrop = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        backdrop.add_css_class("super-desktop-window");
        backdrop.set_size_request(900, 600);
        canvas.put(&backdrop, 0.0, 0.0);

        let hud = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        hud.add_css_class("hud-bar");
        hud.set_size_request(900, 46);
        canvas.put(&hud, 0.0, 0.0);

        let make_card = |x: f64, y: f64, w: i32, h: i32, title: &str| {
            let card = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            card.add_css_class("mini-terminal");
            card.set_size_request(w, h);
            let label = gtk4::Label::new(Some(title));
            label.set_margin_top(20);
            card.append(&label);
            canvas.put(&card, x, y);
        };
        make_card(120.0, 150.0, 400, 300, "💻 buried claude (drawn above)");
        make_card(150.0, 170.0, 350, 260, "🤖 codex");
        window.set_child(Some(&canvas));
        window.present();
        let until = std::time::Instant::now() + std::time::Duration::from_millis(300);
        while std::time::Instant::now() < until {
            while gtk4::glib::MainContext::default().iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let layer = GhostLayer::new(&canvas, &hud, Rc::new(RefCell::new(Vec::<Rc<MiniTerminalCard>>::new())), 900, 600);
        let buried = TerminalPlacement { session: "buried".to_string(), rect: rect_of(120, 150, 400, 300), user_active: false };
        let over = TerminalPlacement { session: "over".to_string(), rect: rect_of(150, 170, 350, 260), user_active: false };
        layer.apply(&[buried, over]);
        let until = std::time::Instant::now() + std::time::Duration::from_millis(400);
        while std::time::Instant::now() < until {
            while gtk4::glib::MainContext::default().iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let snapshot = gtk4::Snapshot::new();
        window.snapshot_child(&canvas, &snapshot);
        let node = snapshot.to_node().expect("render node");
        let renderer = window
            .native()
            .and_then(|native| native.renderer())
            .expect("renderer");
        let texture = renderer.render_texture(node, None);
        texture.save_to_png("/tmp/ghost_preview.png").unwrap();
        println!("PREVIEW written");
    }

    fn rect_of(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect { x: x as f64, y: y as f64, width, height }
    }
}
