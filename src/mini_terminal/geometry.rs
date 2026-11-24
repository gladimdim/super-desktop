use super::*;

pub const CARD_WIDTH: i32 = 380;
pub const CARD_HEIGHT: i32 = 240;
pub const NEW_TERM_WIDTH: i32 = 640;
pub const NEW_TERM_HEIGHT: i32 = 480;
pub const MIN_CARD_WIDTH: i32 = 320;
pub const MIN_CARD_HEIGHT: i32 = 180;
pub const ICON_SIZE: i32 = 128;
/// What every card's footer says when nothing more important is happening.
pub const CARD_HINT: &str = "Double-click to expand • drag any edge to resize";

/// The smallest a card may be in a given workspace's own pixel space.
///
/// A local card's minimum is absolute; a remote card is drawn fitted into the
/// viewer's canvas, so the same host card is smaller on a smaller screen and
/// its minimum has to follow.
pub fn min_card_size(scale: f64) -> (i32, i32) {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    (
        ((MIN_CARD_WIDTH as f64) * scale).round() as i32,
        ((MIN_CARD_HEIGHT as f64) * scale).round() as i32,
    )
}

/// The footer hint: what the card's source wants to say, or the standing
/// invitation to expand and resize.
pub(in crate::mini_terminal) fn card_hint(message: &Rc<RefCell<Option<String>>>) -> String {
    card_hint_or(message, CARD_HINT)
}

pub(in crate::mini_terminal) fn card_hint_or(message: &Rc<RefCell<Option<String>>>, fallback: &str) -> String {
    message
        .borrow()
        .clone()
        .unwrap_or_else(|| fallback.to_string())
}

/// Resize bounds for a card in a `width × height` workspace drawn at `scale`
/// times this machine's pixels: the local rules (10 px margins, 70 px under
/// the dock), scaled with the workspace.
pub fn workspace_limits((width, height): (i32, i32), scale: f64) -> crate::card_resize::Limits {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let (min_width, min_height) = min_card_size(scale);
    let (max_width, max_height) = max_card_size(width, height, scale);
    crate::card_resize::Limits {
        min_width,
        min_height,
        max_width,
        max_height,
        left: 10.0 * scale,
        top: 70.0 * scale,
        right: width as f64 - 10.0 * scale,
        bottom: height as f64 - 10.0 * scale,
    }
}

/// The largest card a `width × height` workspace holds at `scale`: all of it
/// inside the 10 px side and bottom margins and under the 70 px toolbar strip,
/// so an edge can be dragged right up to the screen's margin.
pub fn max_card_size(width: i32, height: i32, scale: f64) -> (i32, i32) {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    (
        (width as f64 - 20.0 * scale).round() as i32,
        (height as f64 - 80.0 * scale).round() as i32,
    )
}

/// Clamp a card size in a workspace whose smallest card is `scale` times this
/// machine's: one rule for the local workspace (scale 1) and a fitted one.
pub fn clamp_card_size_at(w: i32, h: i32, screen_w: i32, screen_h: i32, scale: f64) -> (i32, i32) {
    let (min_w, min_h) = min_card_size(scale);
    let (max_w, max_h) = max_card_size(screen_w, screen_h, scale);
    (
        w.clamp(min_w, max_w.max(min_w)),
        h.clamp(min_h, max_h.max(min_h)),
    )
}

pub const EXPAND_RATIO: f64 = 0.80;
/// Logical pixels outside the last active card that do not activate a neighbour.
pub(in crate::mini_terminal) const HOVER_DEAD_ZONE: f32 = 20.0;

pub(in crate::mini_terminal) fn inside_hover_margin(x: f32, y: f32, width: i32, height: i32) -> bool {
    x >= -HOVER_DEAD_ZONE && y >= -HOVER_DEAD_ZONE
        && x <= width as f32 + HOVER_DEAD_ZONE
        && y <= height as f32 + HOVER_DEAD_ZONE
}

pub fn expanded_rect(screen_w: i32, screen_h: i32) -> (f64, f64, f64, f64) {
    let w = (screen_w as f64 * EXPAND_RATIO).round();
    let h = (screen_h as f64 * EXPAND_RATIO).round();
    let x = ((screen_w as f64 - w) / 2.0).round();
    let y = ((screen_h as f64 - h) / 2.0).round();
    (x, y, w, h)
}

pub fn clamp_card_size(w: i32, h: i32, screen_w: i32, screen_h: i32) -> (i32, i32) {
    let (max_w, max_h) = max_card_size(screen_w, screen_h, 1.0);
    (
        w.clamp(MIN_CARD_WIDTH, max_w.max(MIN_CARD_WIDTH)),
        h.clamp(MIN_CARD_HEIGHT, max_h.max(MIN_CARD_HEIGHT)),
    )
}

/// Each card remembers two independent spots: the expanded card position
/// (`x`/`y`) and the iconified position (`icon_x`/`icon_y`). This returns the
/// spot the widget should occupy right now, so minimizing/restoring can jump
/// between them without either one clobbering the other.
pub fn displayed_pos(data: &TerminalData) -> (f64, f64) {
    if data.iconified {
        (
            data.icon_x.unwrap_or(data.x) as f64,
            data.icon_y.unwrap_or(data.y) as f64,
        )
    } else {
        (data.x as f64, data.y as f64)
    }
}

/// Records a card position in the slot matching its current mode, so moving an
/// icon can never overwrite the expanded card position (and vice versa).
pub fn set_displayed_pos(data: &mut TerminalData, x: i32, y: i32) {
    if data.iconified {
        data.icon_x = Some(x);
        data.icon_y = Some(y);
    } else {
        data.x = x;
        data.y = y;
    }
}

/// A card that was never moved as an icon seeds its icon spot from the card
/// position on the first minimize, so the icon does not jump; after that the
/// saved icon spot is where every minimize returns to.
pub(in crate::mini_terminal) fn seed_icon_pos(data: &mut TerminalData) {
    if data.icon_x.is_none() {
        data.icon_x = Some(data.x);
    }
    if data.icon_y.is_none() {
        data.icon_y = Some(data.y);
    }
}

/// The side of an iconified card in a workspace drawn at `scale`: 128 px on
/// this machine's screen, and a remote PC's 128 logical px fitted into its view.
pub fn icon_side(scale: f64) -> i32 {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    (f64::from(ICON_SIZE) * scale).round().max(1.0) as i32
}

/// The corner radius of a 128 px icon.
pub(in crate::mini_terminal) const ICON_RADIUS: f64 = 18.0;
/// How much larger than 128 px an icon's chrome grows at most (a 100% view
/// zoomed in draws icons up to 3×).
pub(in crate::mini_terminal) const MAX_ICON_SCALE: f64 = 4.0;
/// The largest radius `icon_radius_class` names; styles.rs defines a class for
/// every radius up to it.
pub const MAX_ICON_RADIUS: i32 = (ICON_RADIUS * MAX_ICON_SCALE) as i32;
/// A card's border is at most this wide on each side. The chrome of an icon is
/// fitted inside it.
pub(in crate::mini_terminal) const CARD_BORDER: i32 = 2;
/// Below this the agent's name is noise rather than text, so a small icon
/// shows its logo alone.
pub(in crate::mini_terminal) const MIN_NAME_FONT_PX: f64 = 8.0;

/// The CSS class that rounds an icon `side` px square like a 128 px one.
pub(in crate::mini_terminal) fn icon_radius_class(side: i32) -> String {
    let radius = (ICON_RADIUS * f64::from(side) / f64::from(ICON_SIZE)).round() as i32;
    format!("term-icon-radius-{}", radius.clamp(1, MAX_ICON_RADIUS))
}

/// Set a label's font size in pixels, over the size its CSS gives it.
pub(in crate::mini_terminal) fn set_font_px(label: &Label, px: f64) {
    let attrs = gtk4::pango::AttrList::new();
    let size = (px.max(1.0) * f64::from(gtk4::pango::SCALE)).round() as i32;
    attrs.insert(gtk4::pango::AttrSize::new_size_absolute(size));
    label.set_attributes(Some(&attrs));
}

