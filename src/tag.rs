use gtk4::prelude::*;
use gtk4::{Button, Orientation, Popover, PositionType};

/// Fixed 8-color grouping palette. Index 1..=8, 0 = no tag.
/// Canonical list of the colors also hardcoded as `.tag-dot-N` in styles.rs.
#[allow(dead_code)]
pub const TAG_COLORS: [&str; 8] = [
    "#f87171", // red
    "#fb923c", // orange
    "#facc15", // yellow
    "#4ade80", // green
    "#22d3ee", // cyan
    "#60a5fa", // blue
    "#c084fc", // purple
    "#f472b6", // pink
];

pub const TAG_NONE: u8 = 0;
pub const TAG_COUNT: u8 = 8;
/// Cyan (#22d3ee): the first folder's label color (see `folder_colors`).
pub const TAG_CYAN: u8 = 5;

pub fn normalize_tag(tag: u8) -> u8 {
    if tag >= 1 && tag <= TAG_COUNT {
        tag
    } else {
        TAG_NONE
    }
}

/// CSS class carrying the dot color, e.g. `tag-dot-3` or `tag-dot-none`.
pub fn tag_class(tag: u8) -> String {
    match normalize_tag(tag) {
        TAG_NONE => "tag-dot-none".to_string(),
        n => format!("tag-dot-{n}"),
    }
}

/// Swap the color class on a tag button to reflect `tag`.
pub fn apply_tag(btn: &Button, tag: u8) {
    for n in 1..=TAG_COUNT {
        let cls = format!("tag-dot-{n}");
        if btn.has_css_class(&cls) {
            btn.remove_css_class(&cls);
        }
    }
    if btn.has_css_class("tag-dot-none") {
        btn.remove_css_class("tag-dot-none");
    }
    btn.add_css_class(&tag_class(tag));
}

/// Small color dot for a card header. Clicking it pops up the 8-swatch
/// picker; clicking the active swatch again clears the tag.
/// `on_pick` receives the new tag (0..=8) after the button restyled itself.
pub fn make_tag_dot<F>(initial: u8, on_pick: F) -> Button
where
    F: Fn(u8) + 'static,
{
    let initial = normalize_tag(initial);

    let btn = Button::new();
    btn.set_has_frame(false);
    btn.set_size_request(18, 18);
    btn.set_valign(gtk4::Align::Center);
    btn.add_css_class("tag-dot");
    btn.add_css_class(&tag_class(initial));

    let pop = Popover::new();
    pop.set_position(PositionType::Bottom);
    pop.add_css_class("tag-popover");
    pop.set_parent(&btn);

    let row = gtk4::Box::new(Orientation::Horizontal, 6);
    row.add_css_class("tag-pop-box");

    let on_pick = std::rc::Rc::new(on_pick);
    let current = std::rc::Rc::new(std::cell::Cell::new(initial));
    let mut swatches: Vec<Button> = Vec::with_capacity(TAG_COUNT as usize);

    for n in 1..=TAG_COUNT {
        let sw = Button::new();
        sw.set_has_frame(false);
        sw.set_size_request(24, 24);
        sw.set_valign(gtk4::Align::Center);
        sw.add_css_class("tag-swatch");
        sw.add_css_class(&format!("tag-dot-{n}"));
        if n == initial {
            sw.add_css_class("tag-selected");
        }
        row.append(&sw);
        swatches.push(sw);
    }

    for (idx, sw) in swatches.iter().enumerate() {
        let n = (idx + 1) as u8;
        let btn_w = btn.downgrade();
        let pop_w = pop.downgrade();
        let on_pick = std::rc::Rc::clone(&on_pick);
        let current = std::rc::Rc::clone(&current);
        let swatches_w: Vec<_> = swatches.iter().map(|s| s.downgrade()).collect();
        sw.connect_clicked(move |_| {
            let next = if current.get() == n { TAG_NONE } else { n };
            current.set(next);
            if let Some(b) = btn_w.upgrade() {
                apply_tag(&b, next);
            }
            for (i, w) in swatches_w.iter().enumerate() {
                if let Some(s) = w.upgrade() {
                    if (i + 1) as u8 == next {
                        s.add_css_class("tag-selected");
                    } else {
                        s.remove_css_class("tag-selected");
                    }
                }
            }
            on_pick(next);
            if let Some(p) = pop_w.upgrade() {
                p.popdown();
            }
        });
    }

    let pop_show = pop.downgrade();
    btn.connect_clicked(move |_| {
        if let Some(p) = pop_show.upgrade() {
            p.popup();
        }
    });

    // Detach the popover when the dot is destroyed (card closed), otherwise
    // GTK warns about finalizing a button that still has children.
    let pop_destroy = pop.downgrade();
    btn.connect_destroy(move |_| {
        if let Some(p) = pop_destroy.upgrade() {
            p.unparent();
        }
    });

    pop.set_child(Some(&row));
    btn
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tag_class_mapping() {
        assert_eq!(tag_class(0), "tag-dot-none");
        assert_eq!(tag_class(1), "tag-dot-1");
        assert_eq!(tag_class(8), "tag-dot-8");
    }

    #[test]
    fn test_normalize_tag_clamps() {
        assert_eq!(normalize_tag(0), 0);
        assert_eq!(normalize_tag(5), 5);
        assert_eq!(normalize_tag(9), 0);
        assert_eq!(normalize_tag(255), 0);
    }

    #[test]
    fn test_palette_has_8_entries() {
        assert_eq!(TAG_COLORS.len(), 8);
        for c in TAG_COLORS {
            assert!(c.starts_with('#') && c.len() == 7, "bad color {c}");
        }
    }
}
