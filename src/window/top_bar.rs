use super::*;

pub(in crate::window) fn top_bar_height(size: TopBarSize) -> i32 {
    match size {
        TopBarSize::Small => 36,
        TopBarSize::Medium => 46,
        TopBarSize::Large => HUD_MIN_HEIGHT,
    }
}

/// One bar style for both the local dock and the remote one, so switching PCs
/// never changes the size of the toolbar the user is reading.
pub fn paint_top_bar_size(bar: &impl IsA<gtk4::Widget>, size: TopBarSize) {
    for class in ["hud-size-small", "hud-size-medium", "hud-size-large"] {
        bar.remove_css_class(class);
    }
    bar.add_css_class(match size {
        TopBarSize::Small => "hud-size-small",
        TopBarSize::Medium => "hud-size-medium",
        TopBarSize::Large => "hud-size-large",
    });
    bar.set_height_request(top_bar_height(size));
}

/// The brand on the left of both top bars, and what is left of it when the
/// bar does not fit its display.
pub const BRAND: &str = "⚡ SUPER DESKTOP";
pub const BRAND_ICON: &str = "⚡";

/// A top bar's inside: the left group and the launchers in scrolling
/// viewports, the actions on the right outside every one of them.
pub struct TopBarContent {
    pub widget: gtk4::Box,
    pub(in crate::window) left: gtk4::ScrolledWindow,
    pub(in crate::window) center: gtk4::ScrolledWindow,
    pub(in crate::window) right: gtk4::Widget,
}

impl TopBarContent {
    /// The width the bar needs to show everything at its natural size, with
    /// nothing scrolled: what [`TopBarFit`] compares with the allocation.
    pub fn needed_width(&self) -> i32 {
        let natural = |widget: &gtk4::Widget| widget.measure(Orientation::Horizontal, -1).1;
        natural(self.left.upcast_ref())
            + natural(self.center.upcast_ref())
            + natural(&self.right)
            + self.widget.spacing()
    }
}

/// The controls on the right are outside every scrolling viewport. The left
/// and launcher groups share the remaining space and scroll when necessary.
pub fn top_bar_content(
    left: &impl IsA<gtk4::Widget>,
    launchers: &impl IsA<gtk4::Widget>,
    right: &impl IsA<gtk4::Widget>,
) -> TopBarContent {
    let scroll_group = |child: &gtk4::Widget| {
        let scroll = gtk4::ScrolledWindow::new();
        scroll.set_policy(gtk4::PolicyType::External, gtk4::PolicyType::Never);
        scroll.set_propagate_natural_width(true);
        scroll.set_child(Some(child));
        scroll
    };
    let left = scroll_group(left.as_ref());
    let center = scroll_group(launchers.as_ref());
    let row = gtk4::CenterBox::new();
    row.set_halign(Align::Fill);
    row.set_valign(Align::Fill);
    row.set_start_widget(Some(&left));
    row.set_center_widget(Some(&center));
    // Fixed allocates children at their natural size. Do not let long folder
    // names or many launchers inflate the dock beyond its requested width.
    let viewport = gtk4::ScrolledWindow::new();
    viewport.set_policy(gtk4::PolicyType::External, gtk4::PolicyType::Never);
    viewport.set_hexpand(true);
    viewport.set_vexpand(true);
    viewport.set_child(Some(&row));
    let content = gtk4::Box::new(Orientation::Horizontal, 10);
    content.set_hexpand(true);
    content.set_vexpand(true);
    content.append(&viewport);
    content.append(right);
    TopBarContent { widget: content, left, center, right: right.as_ref().clone() }
}

/// One thing a top bar gives up to fit its display, in the order given up.
pub enum Trim {
    /// The shortcut hint after Hide.
    Hint(Label),
    /// The brand keeps only its icon, and the launchers show icons without
    /// names (their tooltips and hover cards still name them).
    Icons { brand: Label, launchers: Rc<dyn Fn(bool)> },
}

/// Fits a top bar to the width it was allocated.
///
/// Measured, not a breakpoint: whatever the display, the bar shows everything
/// that fits, and gives up its trims one at a time while the natural width of
/// its contents is wider than the bar. A trim comes back as soon as the bar
/// has room for what it would show, after a wider allocation or after a
/// launcher was turned off. Run from the frame clock (`layout`/`after-paint`),
/// so it decides with this frame's allocation and its change lands in the
/// next one; a bar whose width and contents did not change is left alone and
/// so schedules no frame.
pub struct TopBarFit {
    /// Weak: the bar's frame-clock handlers hold this fit.
    pub(in crate::window) bar: glib::WeakRef<gtk4::Box>,
    pub(in crate::window) content: TopBarContent,
    pub(in crate::window) trims: Vec<Trim>,
    /// How many trims are given up right now.
    pub(in crate::window) level: Cell<usize>,
    /// The allocation and the contents' width the last decision was made for.
    pub(in crate::window) decided: Cell<Option<(i32, i32)>>,
}

impl TopBarFit {
    pub fn new(bar: &gtk4::Box, content: TopBarContent, trims: Vec<Trim>) -> Rc<Self> {
        let fit = Rc::new(Self {
            bar: bar.downgrade(),
            content,
            trims,
            level: Cell::new(0),
            decided: Cell::new(None),
        });
        fit.apply(0);
        fit
    }

    /// Compare the contents with the bar's current allocation and show the
    /// most that fits. Called every frame; it changes nothing, and measures
    /// one width, while the bar and its contents stay as they were.
    pub fn fit(&self) {
        let Some(bar) = self.bar.upgrade() else {
            return;
        };
        let available = bar.width();
        if available <= 0 {
            return;
        }
        let need = self.content.needed_width();
        if self.decided.get() == Some((available, need)) {
            return;
        }
        // Try each level in turn, fewest trims first. GTK measures the
        // widgets as they are set here, in this call, so nothing of a level
        // that does not fit is ever drawn.
        let mut chosen = self.trims.len();
        for level in 0..=self.trims.len() {
            self.apply(level);
            if self.content.needed_width() <= available {
                chosen = level;
                break;
            }
        }
        self.apply(chosen);
        self.decided.set(Some((available, self.content.needed_width())));
    }

    #[cfg(test)]
    pub fn level(&self) -> usize {
        self.level.get()
    }

    /// The width the bar needs for what it shows right now.
    #[cfg(test)]
    pub fn needed_width(&self) -> i32 {
        self.content.needed_width()
    }

    /// Show the bar as it is at `level`, to measure what that needs.
    #[cfg(test)]
    pub fn force_level(&self, level: usize) {
        self.apply(level.min(self.trims.len()));
    }

    pub(in crate::window) fn apply(&self, level: usize) {
        self.level.set(level);
        for (index, trim) in self.trims.iter().enumerate() {
            let trimmed = index < level;
            match trim {
                Trim::Hint(hint) => hint.set_visible(!trimmed),
                Trim::Icons { brand, launchers } => {
                    let text = if trimmed { BRAND_ICON } else { BRAND };
                    if brand.text() != text {
                        brand.set_text(text);
                    }
                    launchers(trimmed);
                }
            }
        }
    }
}

/// Run `on_frame` on every frame `widget` is drawn in: at `layout` (after the
/// window's own allocation, since its surface connected first) and again at
/// `after-paint`, the safety net for a frame whose layout ran before the new
/// size was allocated. Event-driven, not a tick callback: a tick callback
/// keeps the frame clock running at the display's refresh rate for as long as
/// the widget lives. A hidden layer surface may be unrealized and come back
/// with a new frame clock, which is followed.
pub fn watch_frames(widget: &impl IsA<gtk4::Widget>, on_frame: Rc<dyn Fn()>) {
    type Connected = Option<(glib::WeakRef<gtk4::gdk::FrameClock>, Vec<glib::SignalHandlerId>)>;
    let connected: Rc<RefCell<Connected>> = Rc::new(RefCell::new(None));
    let disconnect: Rc<dyn Fn()> = {
        let connected = Rc::clone(&connected);
        Rc::new(move || {
            if let Some((clock, handlers)) = connected.borrow_mut().take() {
                if let Some(clock) = clock.upgrade() {
                    for handler in handlers {
                        clock.disconnect(handler);
                    }
                }
            }
        })
    };
    let connect: Rc<dyn Fn(&gtk4::Widget)> = {
        let disconnect = Rc::clone(&disconnect);
        Rc::new(move |widget: &gtk4::Widget| {
            disconnect();
            let Some(clock) = widget.frame_clock() else {
                return;
            };
            let on_layout = Rc::clone(&on_frame);
            let on_paint = Rc::clone(&on_frame);
            let handlers = vec![
                clock.connect_layout(move |_| on_layout()),
                clock.connect_after_paint(move |_| on_paint()),
            ];
            *connected.borrow_mut() = Some((clock.downgrade(), handlers));
            on_frame();
        })
    };
    let widget = widget.as_ref();
    if widget.is_realized() {
        connect(widget);
    }
    widget.connect_realize(move |widget| connect(widget));
    widget.connect_unrealize(move |_| disconnect());
}

/// What plugin QML may ask the overlay for (`sd.call`): the same actions as
/// the user's own clicks.

pub(in crate::window) fn fit_top_bar(hud: &gtk4::Box, width: i32, fit: &TopBarFit) {
    // An unmapped window has no allocation yet. Keep the initial monitor size
    // until the compositor supplies a positive logical width.
    if width <= 0 {
        return;
    }
    if hud.width_request() != width {
        // The bar is allocated the new width in the next frame; its contents
        // are fitted there, against that allocation.
        hud.set_width_request(width);
        return;
    }
    fit.fit();
}

pub(in crate::window) fn bind_top_bar_width(
    hud: &gtk4::Box,
    window: &impl IsA<gtk4::Window>,
    fit: Rc<TopBarFit>,
    on_allocate: Rc<dyn Fn(i32, i32)>,
) {
    // The compositor's allocation is authoritative, never the canvas extents.
    // Every configure (output, scale or window size change) produces a frame,
    // and `watch_frames` runs this on each one; never while unmapped.
    let window = window.as_ref().downgrade();
    let weak = hud.downgrade();
    watch_frames(hud, Rc::new(move || {
        if let (Some(window), Some(hud)) = (window.upgrade(), weak.upgrade()) {
            fit_top_bar(&hud, window.width(), &fit);
            if window.width() > 0 && window.height() > 0 {
                on_allocate(window.width(), window.height());
            }
        }
    }));
}


/// Show the stored toggle shortcut in the HUD: the trailing hint label and the
/// Hide button's tooltip. Called once at build time and again whenever the ⚙
/// Settings panel records a new combination.
pub(in crate::window) fn paint_shortcut_hints(hint: &Label, btn_close: &Button, state: &AppState) {
    let combo = crate::shortcut::current_combo(state.toggle_shortcut.as_deref());
    hint.set_text(&format!("[{combo}]"));
    btn_close.set_tooltip_text(Some(&format!("Hide Super Desktop [{combo} or Esc]")));
}

