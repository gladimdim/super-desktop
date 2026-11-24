use super::*;

/// Everything an iconified card draws: the logo and name in the middle, and the
/// bar with its status, tag, restore and close buttons across the top.
///
/// Its sizes are set here rather than in CSS, in proportion to the icon's side.
/// An icon is 128 px on this machine, but a remote PC's icon is fitted into the
/// view with the rest of that PC's workspace, and chrome kept at full size
/// would hang off a small square. At 128 px these are the icon's usual sizes.
#[derive(Clone)]
pub(in crate::mini_terminal) struct CompactChrome {
    pub(in crate::mini_terminal) card: Overlay,
    pub(in crate::mini_terminal) top_bar: gtk4::Box,
    pub(in crate::mini_terminal) status: Label,
    pub(in crate::mini_terminal) tag: Button,
    pub(in crate::mini_terminal) actions: gtk4::Box,
    pub(in crate::mini_terminal) restore: Button,
    pub(in crate::mini_terminal) close: Button,
    pub(in crate::mini_terminal) icon_box: gtk4::Box,
    pub(in crate::mini_terminal) logo: Option<gtk4::Image>,
    pub(in crate::mini_terminal) glyph: Label,
    pub(in crate::mini_terminal) name: Label,
    /// The side the chrome was last sized for, so laying out the same icon
    /// again changes nothing.
    pub(in crate::mini_terminal) side: Rc<Cell<i32>>,
}

impl CompactChrome {
    /// Size everything for an icon `side` px square.
    pub(in crate::mini_terminal) fn fit(&self, side: i32) {
        let previous = self.side.replace(side);
        if previous == side {
            return;
        }
        self.card.remove_css_class(&icon_radius_class(previous));
        self.card.add_css_class(&icon_radius_class(side));
        // What fits inside the card's border, relative to a 128 px icon's.
        let k = (f64::from(side - 2 * CARD_BORDER) / f64::from(ICON_SIZE - 2 * CARD_BORDER))
            .clamp(0.05, MAX_ICON_SCALE);
        // Rounded down, so the parts never add up to more than the icon.
        let px = |value: f64| (value * k).floor() as i32;
        self.top_bar.set_margin_top(px(6.0));
        self.top_bar.set_margin_start(px(8.0));
        self.top_bar.set_margin_end(px(8.0));
        self.top_bar.set_spacing(px(4.0));
        set_font_px(&self.status, 11.0 * k);
        self.tag.set_size_request(px(18.0), px(18.0));
        // The buttons' margins pad the rounded box behind them.
        self.actions.set_spacing(px(2.0));
        for button in [&self.restore, &self.close] {
            button.set_size_request(px(32.0), px(26.0));
            button.set_margin_top(px(1.0));
            button.set_margin_bottom(px(1.0));
            if let Some(label) = button.child().and_downcast::<Label>() {
                set_font_px(&label, 11.0 * k);
            }
        }
        self.restore.set_margin_start(px(2.0));
        self.close.set_margin_end(px(2.0));
        self.icon_box.set_spacing(px(2.0));
        if let Some(logo) = &self.logo {
            logo.set_pixel_size(px(48.0).max(1));
        }
        // The fallback glyph is sized in points, so it follows the desktop's
        // text scaling as it always has.
        let glyph = gtk4::pango::AttrList::new();
        glyph.insert(gtk4::pango::AttrSize::new(
            (38.0 * k * f64::from(gtk4::pango::SCALE)).round() as i32,
        ));
        self.glyph.set_attributes(Some(&glyph));
        set_font_px(&self.name, 11.0 * k);
        self.name.set_visible(11.0 * k >= MIN_NAME_FONT_PX);
    }
}

/// The parts of a card that change with its form: open, expanded or iconified.
#[derive(Clone)]
pub(in crate::mini_terminal) struct CardChrome {
    pub(in crate::mini_terminal) root: Overlay,
    pub(in crate::mini_terminal) header: gtk4::Box,
    pub(in crate::mini_terminal) footer: gtk4::Box,
    pub(in crate::mini_terminal) preview_label: Label,
    /// Everything the card draws while iconified.
    pub(in crate::mini_terminal) compact: CompactChrome,
    pub(in crate::mini_terminal) expand_btn: Button,
    pub(in crate::mini_terminal) close_btn: Button,
    pub(in crate::mini_terminal) hint_label: Label,
    /// The last thing the source said about this card's session. A chrome
    /// change of our own must not lose it.
    pub(in crate::mini_terminal) source_message: Rc<RefCell<Option<String>>>,
}

impl CardChrome {
    /// Show what the card's current form draws, for a card `width`×`height`.
    pub(in crate::mini_terminal) fn apply(&self, expanded: bool, width: i32, height: i32, iconified: bool, vte_attached: bool) {
        let compact = !expanded && iconified;
        self.header.set_visible(!compact);
        self.footer.set_visible(!compact);
        self.preview_label.set_visible(!compact && !expanded && !vte_attached);
        self.compact.icon_box.set_visible(compact);
        self.compact.top_bar.set_visible(compact && !expanded);
        crate::card_resize::set_resize_borders_visible(&self.root, !expanded && !compact);

        if compact {
            self.compact.fit(width.min(height));
            self.root.add_css_class("term-compact");
        } else {
            self.root.remove_css_class("term-compact");
        }
    }

    /// The card's class, expand button and footer hint for the expanded form
    /// or the card's own.
    pub(in crate::mini_terminal) fn paint_expanded(&self, expanded: bool) {
        if expanded {
            self.root.add_css_class("term-expanded");
            self.expand_btn.set_label("❐");
            self.expand_btn.set_tooltip_text(Some("Collapse back to overlay card"));
            self.hint_label
                .set_label(&card_hint_or(&self.source_message, "Double-click header to collapse"));
        } else {
            self.root.remove_css_class("term-expanded");
            self.expand_btn.set_label("⛶");
            self.expand_btn.set_tooltip_text(Some("Expand to 80% overlay"));
            self.hint_label.set_label(&card_hint(&self.source_message));
        }
    }
}

/// One of the card's own actions, stored after construction so a remote command
/// runs exactly the code the matching local button or gesture runs.
pub(in crate::mini_terminal) type CardAction = Rc<RefCell<Option<Rc<dyn Fn()>>>>;
/// Where a card's icon goes the first time it is minimized, given the card's
/// session and the icon's side; `None` keeps it where the card stood. Set by
/// the local workspace (see `SuperDesktopWindow::first_icon_spot`).
pub(in crate::mini_terminal) type IconSpot = Rc<RefCell<Option<Rc<dyn Fn(&str, i32) -> Option<(i32, i32)>>>>>;
pub(in crate::mini_terminal) type GeometryAction = Rc<RefCell<Option<Rc<dyn Fn(crate::card_resize::Rect)>>>>;

