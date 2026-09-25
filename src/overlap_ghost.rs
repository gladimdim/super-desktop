//! Dotted "there is a terminal under here" outlines for buried cards.
//!
//! Two terminals of the overlay can end up stacked: a card dropped on another
//! one, a freshly launched harness cascading 32px over the previous one, an
//! auto-arrange that stacks a column. The top card hides the one underneath, so
//! the buried card's rectangle is drawn as a dotted ghost outline — the desk
//! still shows that a terminal is there.
//!
//! The rule, in one sentence: **the terminal the user is working in draws no
//! outline of its own, and neither do the terminals it covers; every other
//! buried terminal shows its dotted outline.** "Working in" (see
//! `MiniTerminalCard::user_is_active`) is the pointer being on the card, the
//! card being expanded, or the card having just taken a keystroke / a launch /
//! an expand — all of which expire on their own, so the outlines always come
//! back.
//!
//! Only terminals count as coverers (a sticky note is decoration), and only
//! cards painted *above* a card hide it: a raised card is fully visible and
//! needs no hint about itself.
//!
//! The same stacking also decides which terminals may stop drawing. GTK does
//! no occlusion culling inside a window: a buried terminal that redraws (a
//! spinner) still damages its area, so the card over it is repainted and the
//! compositor re-blurs the region. A terminal that the cards above it hide
//! *completely* ([`hidden_indexes`], a far stricter rule than the outlines'
//! 70%) has its emulator widget hidden until anything uncovers it.
//!
//! The planners ([`ghost_indexes`], [`covered_ratio`], [`hidden_indexes`]) are
//! plain geometry that unit tests cover; [`GhostLayer`] is the thin GTK side
//! that places one outline widget per hidden card and pauses the terminals.

use gtk4::prelude::*;
use gtk4::{Fixed, Widget};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::card_resize::Rect;
use crate::mini_terminal::MiniTerminalCard;

/// Fraction of a card that must be covered, by the cards painted above it, for
/// that card to earn a ghost outline.
pub const OVERLAP_HIDE_RATIO: f64 = 0.70;

/// Corner radius of a card at its roundest: an open or expanded card is 14px,
/// the 128×128 icon 18px (`.mini-terminal` in `styles.rs`). A card's corners
/// are transparent, so whatever lies under one shows through there.
pub const CARD_CORNER_RADIUS: f64 = 18.0;

/// How far a terminal's card may reach past the cards covering it and still
/// count as hidden. The emulator sits well inside its card (the preview box's
/// 8px margin, its border and padding), so a 1px sliver of card edge never
/// shows terminal output; it absorbs rounding in the geometry.
pub const COVER_TOLERANCE: f64 = 1.0;

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

/// One terminal as the draw planner sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawPlacement {
    /// Where the card is drawn on the overlay canvas.
    pub rect: Rect,
    /// This terminal draws whatever covers it: the user is working in it, it
    /// holds the keyboard focus, or it cannot pause yet (see
    /// `MiniTerminalCard::vte_can_pause`).
    pub keep_drawing: bool,
}

/// Indexes into `placements` (bottom-most first) whose terminal the cards
/// painted above it hide completely, so it can stop drawing.
///
/// Unlike [`ghost_indexes`] a coverer's own activity does not matter: the
/// terminals under the one somebody is typing in, or under an expanded card,
/// are exactly the ones whose redraws cost the most.
pub fn hidden_indexes(placements: &[DrawPlacement]) -> Vec<usize> {
    let mut hidden = Vec::new();
    for (index, card) in placements.iter().enumerate() {
        if card.keep_drawing {
            continue;
        }
        let coverers: Vec<Rect> = placements[index + 1..]
            .iter()
            .map(|coverer| coverer.rect)
            .collect();
        if fully_covered(card.rect, &coverers) {
            hidden.push(index);
        }
    }
    hidden
}

/// True when `coverers` hide every pixel of `target`, give or take
/// [`COVER_TOLERANCE`] along its edges.
///
/// A coverer only counts where it is opaque for sure: its rectangle without
/// the [`CARD_CORNER_RADIUS`] square at each corner, since a rounded corner is
/// transparent and whatever is under it shows through. Growing the target by
/// the radius instead is not enough: four cards meeting in the middle of a
/// fifth leave a hole where their corners meet. The shadows cards cast are
/// see-through and never count either.
pub fn fully_covered(target: Rect, coverers: &[Rect]) -> bool {
    let inner = Area {
        x0: target.x + COVER_TOLERANCE,
        y0: target.y + COVER_TOLERANCE,
        x1: target.x + target.width as f64 - COVER_TOLERANCE,
        y1: target.y + target.height as f64 - COVER_TOLERANCE,
    };
    let Some(needed) = inner.size() else {
        return false;
    };
    let opaque: Vec<Area> = coverers
        .iter()
        .flat_map(|coverer| Area::opaque_parts(*coverer))
        .filter_map(|part| part.clip(&inner))
        .collect();
    // Integer card geometry makes the union exact; the epsilon is only there
    // for the floating-point sum of the slabs.
    Area::union(&opaque) >= needed - 1e-6
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
    fn from_rect(rect: Rect) -> Self {
        Self {
            x0: rect.x,
            y0: rect.y,
            x1: rect.x + rect.width as f64,
            y1: rect.y + rect.height as f64,
        }
    }

    fn intersection(a: Rect, b: Rect) -> Option<Self> {
        Self::from_rect(a).clip(&Self::from_rect(b))
    }

    /// The part of `self` inside `other`, if any.
    fn clip(&self, other: &Area) -> Option<Self> {
        let area = Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        area.size().map(|_| area)
    }

    /// Area in square pixels, or `None` when the rectangle is empty.
    fn size(&self) -> Option<f64> {
        (self.x1 > self.x0 && self.y1 > self.y0).then(|| (self.x1 - self.x0) * (self.y1 - self.y0))
    }

    /// The card's surely opaque region, as two overlapping bands: the "plus"
    /// left once the [`CARD_CORNER_RADIUS`] square at each corner is cut off.
    fn opaque_parts(rect: Rect) -> [Area; 2] {
        let card = Self::from_rect(rect);
        let r = CARD_CORNER_RADIUS;
        [
            Self {
                x0: card.x0 + r,
                x1: card.x1 - r,
                ..card
            },
            Self {
                y0: card.y0 + r,
                y1: card.y1 - r,
                ..card
            },
        ]
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
    /// not hang in mid-air at rest poses the cards have not reached yet. No
    /// terminal is paused meanwhile either, since the cards move on their own
    /// paths and uncover each other.
    live: Cell<bool>,
    /// Set while every terminal must draw whatever covers it: the Alt picker
    /// dims the cards, and the terminals under them show through.
    all_drawing: Cell<bool>,
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
            all_drawing: Cell::new(false),
            outlines: RefCell::new(Vec::new()),
        })
    }

    /// Recompute the outlines, and which terminals draw, from the live cards
    /// and their stacking order.
    pub fn refresh(&self) {
        if !self.live.get() {
            return;
        }
        let cards = self.cards.borrow().clone();
        let ordered = paint_order(&self.canvas, &cards);
        let placements: Vec<TerminalPlacement> = ordered
            .iter()
            .map(|card| TerminalPlacement {
                session: card.data.borrow().session_name.clone(),
                rect: card.canvas_rect(self.screen_w, self.screen_h),
                user_active: card.user_is_active(),
            })
            .collect();
        self.apply(&placements);
        self.pause_hidden_terminals(&ordered);
    }

    /// Stop drawing the terminals that the cards above them hide completely,
    /// and let every other terminal draw.
    ///
    /// Nothing pauses while a card is dragged: what it covers changes on every
    /// frame, and hiding and re-mapping terminals as it passes over them would
    /// cost more than it saves. The drag's end refreshes and pauses again. A
    /// resize only moves the dashed preview; the card itself changes once, at
    /// the commit, which refreshes too.
    fn pause_hidden_terminals(&self, ordered: &[Rc<MiniTerminalCard>]) {
        let all_drawing =
            self.all_drawing.get() || ordered.iter().any(|card| card.is_being_dragged());
        let hidden = if all_drawing {
            Vec::new()
        } else {
            let placements: Vec<DrawPlacement> = ordered
                .iter()
                .map(|card| {
                    let rect = card.canvas_rect(self.screen_w, self.screen_h);
                    DrawPlacement {
                        rect,
                        keep_drawing: card.user_is_active()
                            || !card.vte_can_pause(&self.canvas, rect),
                    }
                })
                .collect();
            hidden_indexes(&placements)
        };
        for (index, card) in ordered.iter().enumerate() {
            card.set_vte_covered(hidden.contains(&index));
        }
    }

    /// Let every terminal draw again, whatever covers it.
    fn draw_all_terminals(&self) {
        let cards = self.cards.borrow().clone();
        for card in cards.iter() {
            card.set_vte_covered(false);
        }
    }

    /// Keep every terminal drawing while `keep` is set (the Alt picker is up),
    /// and go back to pausing the hidden ones once it is cleared.
    pub fn keep_all_drawing(&self, keep: bool) {
        self.all_drawing.set(keep);
        if keep {
            self.draw_all_terminals();
        } else {
            self.refresh();
        }
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
                    Some(index) => outlines[index].show(&self.canvas, *rect),
                    None => {
                        let outline = Outline::new(session, &self.canvas, *rect);
                        outline.show(&self.canvas, *rect);
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

    /// Take every outline off screen, let every terminal draw, and stop
    /// reacting to card changes.
    pub fn suspend(&self) {
        self.live.set(false);
        for outline in self.outlines.borrow().iter() {
            outline.hide();
        }
        self.draw_all_terminals();
    }

    /// Allow outlines again (the slide settled) and redraw them; the hidden
    /// terminals pause again.
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
        outline.place(canvas, rect);
        outline
    }

    fn show(&self, canvas: &Fixed, rect: Rect) {
        if self.rect.get() == Some(rect) {
            if !self.widget.is_visible() {
                self.widget.set_visible(true);
            }
            return;
        }
        if self.widget.is_visible() {
            canvas.move_(&self.widget, rect.x, rect.y);
        } else {
            // Show before sizing, and re-add the widget: a `GtkFixed` child that
            // is off screen keeps its old allocation when its size request
            // changes, so an outline that was hidden while its card was resized
            // came back painted at the card's *previous* size. Re-adding it
            // forces a fresh measure — the same trick the resize preview uses.
            canvas.remove(&self.widget);
            canvas.put(&self.widget, rect.x, rect.y);
        }
        self.widget.set_visible(true);
        self.widget.set_size_request(rect.width, rect.height);
        self.rect.set(Some(rect));
    }

    fn hide(&self) {
        // Forget the geometry so the next show re-places the outline: the card
        // may have been dragged, arranged or resized meanwhile.
        self.rect.set(None);
        if self.widget.is_visible() {
            self.widget.set_visible(false);
        }
    }

    fn place(&self, canvas: &Fixed, rect: Rect) {
        if self.rect.get() == Some(rect) {
            return;
        }
        canvas.move_(&self.widget, rect.x, rect.y);
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

    fn terminal(rect: Rect) -> DrawPlacement {
        DrawPlacement {
            rect,
            keep_drawing: false,
        }
    }

    /// A card `margin` pixels larger than `target` on every side.
    fn around(target: Rect, margin: i32) -> Rect {
        rect(
            target.x as i32 - margin,
            target.y as i32 - margin,
            target.width + 2 * margin,
            target.height + 2 * margin,
        )
    }

    #[test]
    fn a_terminal_pauses_only_when_it_is_covered_completely() {
        let under = rect(100, 100, 400, 300);
        // Far enough past every edge that the coverer's round corners are
        // outside the card underneath.
        assert_eq!(
            hidden_indexes(&[terminal(under), terminal(around(under, 40))]),
            [0]
        );
        // 99% is not hidden: the strip still shows live output.
        let almost = rect(60, 60, 437, 380);
        assert!(covered_ratio(under, &[almost]) > 0.99);
        assert!(hidden_indexes(&[terminal(under), terminal(almost)]).is_empty());
        // The 70% that earns a ghost outline keeps the terminal drawing.
        let most = rect(100, 100, 288, 300);
        assert!(covered_ratio(under, &[most]) >= OVERLAP_HIDE_RATIO);
        assert!(hidden_indexes(&[terminal(under), terminal(most)]).is_empty());
        // Nothing above it, nothing hidden.
        assert!(hidden_indexes(&[terminal(under)]).is_empty());
        assert!(!fully_covered(under, &[]));
    }

    #[test]
    fn only_cards_painted_above_pause_a_terminal() {
        let small = rect(140, 140, 320, 220);
        let big = rect(100, 100, 400, 300);
        assert_eq!(hidden_indexes(&[terminal(small), terminal(big)]), [0]);
        // The big card is painted over the small one's rectangle, not under it.
        assert!(hidden_indexes(&[terminal(big), terminal(small)]).is_empty());
    }

    #[test]
    fn two_cards_together_can_pause_a_terminal() {
        let under = rect(100, 100, 400, 300);
        // Each covers a little over half of it and reaches past the outer
        // edges: their inner corners lie above and below the card.
        let left = rect(60, 60, 250, 380);
        let right = rect(290, 60, 250, 380);
        assert_eq!(
            hidden_indexes(&[terminal(under), terminal(left), terminal(right)]),
            [0]
        );
        // A 2px gap between them shows a column of the terminal.
        let apart = rect(312, 60, 250, 380);
        assert!(hidden_indexes(&[terminal(under), terminal(left), terminal(apart)]).is_empty());
    }

    #[test]
    fn a_coverer_s_round_corners_let_the_terminal_show_through() {
        let under = rect(100, 100, 400, 300);
        // Exactly the same rectangle: its four transparent corners are over the
        // card underneath.
        assert!(!fully_covered(under, &[under]));
        // Past every edge by the corner radius, the corners no longer matter.
        let radius = CARD_CORNER_RADIUS as i32;
        assert!(fully_covered(under, &[around(under, radius)]));
        assert!(!fully_covered(under, &[around(under, radius - 2)]));
        // Four cards meeting in the middle cover every pixel as rectangles,
        // but their corners leave a hole where they meet.
        let quarters = [
            rect(60, 60, 240, 190),
            rect(300, 60, 240, 190),
            rect(60, 250, 240, 190),
            rect(300, 250, 240, 190),
        ];
        assert_eq!(covered_ratio(under, &quarters), 1.0);
        assert!(!fully_covered(under, &quarters));
        // Overlapping by more than the radius, they close the hole again.
        let overlapping = [
            rect(60, 60, 260, 210),
            rect(280, 60, 260, 210),
            rect(60, 230, 260, 210),
            rect(280, 230, 260, 210),
        ];
        assert!(fully_covered(under, &overlapping));
    }

    #[test]
    fn a_one_pixel_sliver_still_counts_as_covered() {
        let over = rect(60, 60, 480, 380);
        // One pixel past the coverer's left edge: that is card border.
        assert!(fully_covered(rect(59, 100, 400, 300), &[over]));
        assert!(fully_covered(rect(141, 100, 400, 300), &[over]));
        // Two pixels are not.
        assert!(!fully_covered(rect(58, 100, 400, 300), &[over]));
        assert!(!fully_covered(rect(100, 142, 400, 300), &[over]));
        // A card too thin to hold a terminal is never paused.
        assert!(!fully_covered(rect(100, 100, 2, 300), &[over]));
    }

    #[test]
    fn an_expanded_card_pauses_every_terminal_it_hides() {
        let (x, y, width, height) = crate::mini_terminal::expanded_rect(2560, 1440);
        let expanded = DrawPlacement {
            rect: Rect {
                x,
                y,
                width: width as i32,
                height: height as i32,
            },
            // Expanded counts as the user working in it.
            keep_drawing: true,
        };
        let inside = terminal(rect(600, 400, 640, 480));
        let icon = terminal(rect(1800, 900, 128, 128));
        // Reaches above the expanded card: its title bar is on screen.
        let straddling = terminal(rect(1200, 100, 640, 480));
        let placements = [inside, straddling, icon, expanded];
        assert_eq!(hidden_indexes(&placements), [0, 2]);

        // A card raised over the expanded one draws, and hides what it covers
        // even where the expanded card does not reach.
        let raised = terminal(rect(200, 300, 400, 300));
        let under_raised = terminal(rect(230, 340, 200, 150));
        assert!(!fully_covered(under_raised.rect, &[expanded.rect]));
        assert_eq!(
            hidden_indexes(&[under_raised, expanded, raised]),
            [0],
            "the expanded card and the raised card both draw"
        );
    }

    #[test]
    fn the_terminal_in_use_keeps_drawing_under_anything() {
        let under = rect(100, 100, 400, 300);
        let over = terminal(around(under, 40));
        let in_use = DrawPlacement {
            rect: under,
            keep_drawing: true,
        };
        assert!(hidden_indexes(&[in_use, over]).is_empty());
        // The coverer being in use does not matter: that is when the buried
        // terminals' redraws cost the most.
        let over_in_use = DrawPlacement {
            keep_drawing: true,
            ..over
        };
        assert_eq!(hidden_indexes(&[terminal(under), over_in_use]), [0]);
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

    #[test]
    fn buried_terminals_pause_and_draw_again_on_every_trigger() {
        crate::gtk_test::run_in_child_process("overlap_ghost::tests::pause_gtk");
    }

    /// The wiring from the overlay's triggers to real cards' terminals: a raise,
    /// the whole-window hide and show, a slide, the Alt picker, a drag, a move,
    /// the keyboard focus, an expand and its collapse, a card that has not been
    /// laid out yet, and a close.
    #[test]
    fn pause_gtk() {
        if !crate::gtk_test::is_child() {
            return;
        }
        // A card asks tmux for its status (when it is made and collapsed). Point
        // tmux at a directory that does not exist: it cannot create its socket
        // directory there, so every probe fails without reaching the user's
        // server or leaving anything on disk.
        let no_tmux = std::env::temp_dir()
            .join(format!("sd-pause-test-{}", std::process::id()))
            .join("missing");
        std::env::remove_var("TMUX");
        std::env::remove_var("TMUX_PANE");
        std::env::set_var("TMUX_TMPDIR", &no_tmux);

        gtk4::init().unwrap();
        crate::styles::apply_styles();
        // Broadway runs few frames, and a terminal shown again only has a size
        // after the next one: wait for what a test step needs.
        let settle = |done: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !done() && std::time::Instant::now() < deadline {
                while gtk4::glib::MainContext::default().iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(done(), "GTK never got there");
        };
        let laid_out = |card: &Rc<MiniTerminalCard>| {
            card.vte_widget().is_some_and(|term| term.width() > 0)
        };
        let (screen_w, screen_h) = (1000, 700);
        let window = gtk4::Window::new();
        let canvas = Fixed::new();
        canvas.set_size_request(screen_w, screen_h);
        let hud = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        canvas.put(&hud, 0.0, 0.0);
        window.set_child(Some(&canvas));
        let cards: Rc<RefCell<Vec<Rc<MiniTerminalCard>>>> = Rc::new(RefCell::new(Vec::new()));
        let layer = GhostLayer::new(&canvas, &hud, Rc::clone(&cards), screen_w, screen_h);

        // Raise like the window does: to the top of the canvas, under the
        // toolbar, then recompute.
        let raise = {
            let canvas = canvas.clone();
            let hud = hud.clone();
            let layer = Rc::downgrade(&layer);
            Rc::new(move |widget: Widget| {
                crate::window::raise_canvas_child(&canvas, &widget);
                crate::window::raise_canvas_child(&canvas, &hud);
                if let Some(layer) = layer.upgrade() {
                    layer.refresh();
                }
            })
        };
        let make = |session: &str, x: i32, y: i32, width: i32, height: i32| {
            let on_raise = Rc::clone(&raise);
            let touched = Rc::downgrade(&layer);
            let data = crate::state::TerminalData {
                id: session.to_string(),
                session_name: session.to_string(),
                agent_type: "shell".to_string(),
                command: "/usr/bin/bash".to_string(),
                x,
                y,
                width,
                height,
                restored_width: width,
                restored_height: height,
                // Iconified cards start without an emulator (and so without a
                // tmux session); the bare one below runs nothing.
                iconified: true,
                icon_x: None,
                icon_y: None,
                created_at: 0.0,
                tag: 0,
                agent_session_id: None,
                workspace_dir: None,
            };
            let card = Rc::new(MiniTerminalCard::new(
                data,
                |_, _, _| {},
                |_, _| {},
                |_| {},
                |_| {},
                |_, _, _, _, _| {},
                || {},
                move |widget| on_raise(widget),
                |_| {},
                move || {
                    if let Some(layer) = touched.upgrade() {
                        layer.refresh();
                    }
                },
                screen_w,
                screen_h,
                None,
                Some(Rc::new(Vec::new())),
                crate::mini_terminal::HoverRaiseLock::new(),
                crate::card_source::CardSource::Local,
            ));
            card.open_with_bare_terminal(width, height);
            canvas.put(&card.container, x as f64, y as f64);
            crate::window::raise_canvas_child(&canvas, &hud);
            cards.borrow_mut().push(Rc::clone(&card));
            card
        };
        let raise_card = |card: &Rc<MiniTerminalCard>| raise(card.container.clone().upcast());
        let move_card = |card: &Rc<MiniTerminalCard>, x: i32, y: i32| {
            crate::mini_terminal::set_displayed_pos(&mut card.data.borrow_mut(), x, y);
            canvas.move_(&card.container, x as f64, y as f64);
        };

        // `top` hides `bottom` with 40px to spare on every side; `side` and
        // `small` are in the open.
        let bottom = make("sd-pause-bottom", 100, 120, 400, 300);
        let side = make("sd-pause-side", 600, 120, 360, 300);
        let top = make("sd-pause-top", 60, 80, 480, 380);
        let small = make("sd-pause-small", 520, 440, 360, 180);
        let all = [&bottom, &side, &top, &small];
        window.present();
        settle(&|| all.iter().all(|card| laid_out(card)));
        // Mapping may have focused a terminal, which raises its card.
        gtk4::prelude::GtkWindowExt::set_focus(&window, None::<&Widget>);
        for card in all {
            crate::window::raise_canvas_child(&canvas, &card.container);
        }
        crate::window::raise_canvas_child(&canvas, &hud);

        // Laid out at its own size (the fractional border adds a pixel).
        let bounds = |card: &Rc<MiniTerminalCard>| {
            let bounds = card.container.compute_bounds(&canvas).unwrap();
            (bounds.width(), bounds.height())
        };
        let size = bounds(&bottom);
        assert!((size.0 - 400.0).abs() <= 2.0 && (size.1 - 300.0).abs() <= 2.0, "{size:?}");
        layer.refresh();
        assert!(!bottom.vte_visible(), "a terminal hidden completely stops drawing");
        assert!(side.vte_visible() && top.vte_visible() && small.vte_visible());
        settle(&|| !bottom.vte_widget().unwrap().is_mapped());
        assert_eq!(bounds(&bottom), size, "pausing does not resize the card");

        raise_card(&bottom);
        assert!(bottom.vte_visible(), "a raise brings it back at once");
        // Buried again before it was laid out: it has no size until the next
        // frame, so it pauses at the next refresh (the overlay runs one every
        // second), never before.
        raise_card(&top);
        assert!(bottom.vte_visible());
        settle(&|| laid_out(&bottom));
        layer.refresh();
        assert!(!bottom.vte_visible());

        // The overlay is hidden and shown while the card is buried.
        for card in all {
            card.set_vte_drawing(false);
        }
        assert!(all.iter().all(|card| !card.vte_visible()));
        for card in all {
            card.set_vte_drawing(true);
        }
        assert!(!bottom.vte_visible(), "showing the overlay keeps it paused");
        assert!(side.vte_visible() && top.vte_visible() && small.vte_visible());

        // A slide moves the cards on their own paths: everything draws until
        // they are at rest.
        layer.suspend();
        assert!(bottom.vte_visible());
        settle(&|| laid_out(&bottom));
        layer.refresh();
        assert!(bottom.vte_visible(), "nothing pauses mid-slide");
        layer.resume();
        assert!(!bottom.vte_visible());

        // The Alt picker dims every card.
        layer.keep_all_drawing(true);
        assert!(bottom.vte_visible());
        settle(&|| laid_out(&bottom));
        layer.refresh();
        assert!(bottom.vte_visible(), "nothing pauses under the picker");
        layer.keep_all_drawing(false);
        assert!(!bottom.vte_visible());

        // A drag draws everything until it ends.
        top.container.add_css_class("dragging");
        layer.refresh();
        assert!(bottom.vte_visible(), "nothing pauses during a drag");
        settle(&|| laid_out(&bottom));
        layer.refresh();
        assert!(bottom.vte_visible());
        top.container.remove_css_class("dragging");
        layer.refresh();
        assert!(!bottom.vte_visible());

        // Moving the coverer so that a strip of the card shows.
        move_card(&top, 140, 80);
        layer.refresh();
        assert!(bottom.vte_visible(), "a visible strip means drawing");
        settle(&|| laid_out(&bottom));
        move_card(&top, 60, 80);
        layer.refresh();
        assert!(!bottom.vte_visible());

        // Hiding the terminal that holds the keyboard would drop the focus.
        raise_card(&bottom);
        settle(&|| laid_out(&bottom));
        bottom.vte_widget().unwrap().grab_focus();
        assert!(bottom.vte_widget().unwrap().is_focus());
        raise_card(&top);
        assert!(bottom.vte_visible(), "the focused terminal keeps drawing");
        gtk4::prelude::GtkWindowExt::set_focus(&window, None::<&Widget>);
        layer.refresh();
        assert!(!bottom.vte_visible());

        // Expanding a card pauses what it hides, like the window's expand:
        // the card grows, then moves to the top of the canvas.
        side.expand(screen_w, screen_h);
        let (x, y, _, _) = crate::mini_terminal::expanded_rect(screen_w, screen_h);
        canvas.remove(&side.container);
        canvas.put(&side.container, x, y);
        crate::window::raise_canvas_child(&canvas, &hud);
        layer.refresh();
        assert!(side.vte_visible(), "the expanded card draws");
        assert!(!small.vte_visible(), "a card inside the expanded one pauses");
        assert!(top.vte_visible(), "a card reaching past it keeps drawing");
        side.collapse();
        move_card(&side, 600, 120);
        layer.refresh();
        assert!(small.vte_visible(), "collapsing brings it back");
        assert!(!bottom.vte_visible());

        // A card that was never laid out has not sized its PTY yet: it is not
        // paused until it has drawn once.
        let fresh = make("sd-pause-fresh", 120, 140, 360, 240);
        raise_card(&top);
        assert!(fresh.vte_visible(), "a terminal that never drew is not paused");
        settle(&|| laid_out(&fresh));
        // Broadway may hand the newly mapped card a pointer crossing, which
        // raises it: put the coverer back on top once it has drawn.
        raise_card(&top);
        assert!(!fresh.vte_visible(), "once it has drawn, it pauses");

        // Closing the coverer sets everything under it free.
        cards.borrow_mut().retain(|card| !Rc::ptr_eq(card, &top));
        canvas.remove(&top.container);
        layer.refresh();
        assert!(bottom.vte_visible() && fresh.vte_visible());

        window.close();
        assert!(!no_tmux.exists(), "no tmux server may start for this test");
    }

    #[test]
    fn an_outline_follows_its_card_after_a_resize() {
        crate::gtk_test::run_in_child_process_needing_large_screen("overlap_ghost::tests::outline_resize_gtk");
    }

    #[test]
    fn outline_resize_gtk() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        let app = gtk4::Application::new(
            Some("com.superdesktop.GhostResizeTest"),
            gtk4::gio::ApplicationFlags::NON_UNIQUE,
        );
        app.register(None::<&gtk4::gio::Cancellable>).unwrap();
        let window = gtk4::ApplicationWindow::new(&app);
        let canvas = Fixed::new();
        canvas.set_size_request(1400, 900);
        let hud = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        canvas.put(&hud, 0.0, 0.0);
        let coverer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        canvas.put(&coverer, 0.0, 0.0);
        window.set_child(Some(&canvas));
        window.present();

        let layer = GhostLayer::new(
            &canvas,
            &hud,
            Rc::new(RefCell::new(Vec::<Rc<MiniTerminalCard>>::new())),
            1400,
            900,
        );
        let buried = |x: i32, y: i32, width: i32, height: i32| TerminalPlacement {
            session: "buried".to_string(),
            rect: rect(x, y, width, height),
            user_active: false,
        };
        let coverer_at = |x: i32| TerminalPlacement {
            session: "coverer".to_string(),
            rect: rect(x, 120, 600, 420),
            user_active: false,
        };

        layer.apply(&[buried(100, 100, 640, 480), coverer_at(120)]);
        pump();
        assert_eq!(outlines(&canvas)[0].size_request(), (640, 480));

        // The covering card is dragged away, so the outline goes off screen…
        layer.apply(&[buried(100, 100, 640, 480), coverer_at(900)]);
        pump();
        assert!(!outlines(&canvas)[0].is_visible());

        // …and the buried card is resized while its outline is hidden. When the
        // outline comes back it must be the card's *current* size: a `GtkFixed`
        // child that is off screen keeps its old allocation when its size
        // request changes, which used to paint the previous, larger box. The
        // allocation is what matters here — the size request was right all along.
        layer.apply(&[buried(200, 260, 335, 242), coverer_at(220)]);
        pump();
        let outline = &outlines(&canvas)[0];
        assert!(outline.is_visible());
        assert_eq!(canvas.child_position(outline), (200.0, 260.0));
        assert_eq!(outline.size_request(), (335, 242));
        assert_eq!(
            (outline.width(), outline.height()),
            (335, 242),
            "the outline must not keep the allocation of the previous size"
        );
    }
}
