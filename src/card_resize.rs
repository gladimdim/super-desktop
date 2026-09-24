use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Align, GestureDrag, Overlay};
use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
    NorthWest,
}

impl Edge {
    pub const ALL: [Self; 8] = [
        Self::North,
        Self::NorthEast,
        Self::East,
        Self::SouthEast,
        Self::South,
        Self::SouthWest,
        Self::West,
        Self::NorthWest,
    ];

    fn west(self) -> bool {
        matches!(self, Self::West | Self::NorthWest | Self::SouthWest)
    }

    fn east(self) -> bool {
        matches!(self, Self::East | Self::NorthEast | Self::SouthEast)
    }

    fn north(self) -> bool {
        matches!(self, Self::North | Self::NorthEast | Self::NorthWest)
    }

    fn south(self) -> bool {
        matches!(self, Self::South | Self::SouthEast | Self::SouthWest)
    }

    fn cursor(self) -> &'static str {
        match self {
            Self::North | Self::South => "ns-resize",
            Self::East | Self::West => "ew-resize",
            Self::NorthWest | Self::SouthEast => "nwse-resize",
            Self::NorthEast | Self::SouthWest => "nesw-resize",
        }
    }
}

#[derive(Clone, Copy)]
pub struct Limits {
    pub min_width: i32,
    pub min_height: i32,
    pub max_width: i32,
    pub max_height: i32,
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

/// Resize a rectangle while keeping the edge opposite the pointer fixed.
/// Bounds are applied to both the moving edge and the resulting dimensions.
pub fn resized_rect(edge: Edge, start: Rect, dx: f64, dy: f64, limits: Limits) -> Rect {
    let start_right = start.x + start.width as f64;
    let start_bottom = start.y + start.height as f64;
    let min_w = limits.min_width as f64;
    let min_h = limits.min_height as f64;
    let max_w = limits.max_width.max(limits.min_width) as f64;
    let max_h = limits.max_height.max(limits.min_height) as f64;

    let (x, width) = if edge.west() {
        let x = (start.x + dx).clamp((start_right - max_w).max(limits.left), start_right - min_w);
        (x, start_right - x)
    } else if edge.east() {
        let right = (start_right + dx).clamp(start.x + min_w, (start.x + max_w).min(limits.right));
        (start.x, right - start.x)
    } else {
        (start.x, start.width as f64)
    };

    let (y, height) = if edge.north() {
        let y = (start.y + dy).clamp((start_bottom - max_h).max(limits.top), start_bottom - min_h);
        (y, start_bottom - y)
    } else if edge.south() {
        let bottom =
            (start_bottom + dy).clamp(start.y + min_h, (start.y + max_h).min(limits.bottom));
        (start.y, bottom - start.y)
    } else {
        (start.y, start.height as f64)
    };

    Rect {
        x: x.round(),
        y: y.round(),
        width: width.round() as i32,
        height: height.round() as i32,
    }
}

/// Add Windows-style resize targets around an overlay. Motion is coalesced on
/// its frame clock, so a 1000 Hz mouse still causes at most one layout per frame.
pub fn attach_resize_borders(
    root: &Overlay,
    limits: Limits,
    get_start: Rc<dyn Fn() -> Option<Rect>>,
    on_begin: Rc<dyn Fn()>,
    on_preview: Rc<dyn Fn(Rect)>,
    on_commit: Rc<dyn Fn(Rect)>,
) {
    attach_resize_borders_with(root, Rc::new(move || limits), get_start, on_begin, on_preview, on_commit);
}

/// The same targets, with bounds read when each resize begins: a workspace
/// whose size or scale changes while the card lives (a remote view switching
/// between Fit and 100%, or zooming) keeps resizing inside its current bounds.
pub fn attach_resize_borders_with(
    root: &Overlay,
    limits_now: Rc<dyn Fn() -> Limits>,
    get_start: Rc<dyn Fn() -> Option<Rect>>,
    on_begin: Rc<dyn Fn()>,
    on_preview: Rc<dyn Fn(Rect)>,
    on_commit: Rc<dyn Fn(Rect)>,
) {
    for edge in Edge::ALL {
        let limits_cell = Rc::new(Cell::new(limits_now()));
        let zone = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        zone.add_css_class("card-resize-zone");
        zone.set_cursor_from_name(Some(edge.cursor()));
        zone.set_can_target(true);
        place_zone(&zone, edge);
        root.add_overlay(&zone);

        let start = Rc::new(Cell::new(None::<Rect>));
        let pending = Rc::new(Cell::new(None::<Rect>));
        let tick_active = Rc::new(Cell::new(false));
        let drag = GestureDrag::new();
        drag.set_propagation_phase(gtk4::PropagationPhase::Capture);

        let start_begin = Rc::clone(&start);
        let get_start_begin = Rc::clone(&get_start);
        let on_begin_begin = Rc::clone(&on_begin);
        let limits_begin = Rc::clone(&limits_cell);
        let limits_source = Rc::clone(&limits_now);
        drag.connect_drag_begin(move |gesture, _, _| {
            limits_begin.set(limits_source());
            let rect = get_start_begin();
            start_begin.set(rect);
            if rect.is_some() {
                gesture.set_state(gtk4::EventSequenceState::Claimed);
                on_begin_begin();
            } else {
                gesture.set_state(gtk4::EventSequenceState::Denied);
            }
        });

        let start_update = Rc::clone(&start);
        let pending_update = Rc::clone(&pending);
        let tick_update = Rc::clone(&tick_active);
        let preview_update = Rc::clone(&on_preview);
        let root_weak = root.downgrade();
        let limits_update = Rc::clone(&limits_cell);
        drag.connect_drag_update(move |_, dx, dy| {
            let Some(initial) = start_update.get() else {
                return;
            };
            pending_update.set(Some(resized_rect(edge, initial, dx, dy, limits_update.get())));
            if tick_update.replace(true) {
                return;
            }
            let pending_tick = Rc::clone(&pending_update);
            let active_tick = Rc::clone(&tick_update);
            let preview_tick = Rc::clone(&preview_update);
            let Some(root) = root_weak.upgrade() else {
                active_tick.set(false);
                return;
            };
            root.add_tick_callback(move |_, _| {
                if let Some(rect) = pending_tick.take() {
                    preview_tick(rect);
                    glib::ControlFlow::Continue
                } else {
                    active_tick.set(false);
                    glib::ControlFlow::Break
                }
            });
        });

        let start_end = Rc::clone(&start);
        let pending_end = Rc::clone(&pending);
        let commit_end = Rc::clone(&on_commit);
        let limits_end = Rc::clone(&limits_cell);
        drag.connect_drag_end(move |_, dx, dy| {
            let Some(initial) = start_end.take() else {
                return;
            };
            pending_end.set(None);
            commit_end(resized_rect(edge, initial, dx, dy, limits_end.get()));
        });
        zone.add_controller(drag);
    }
}

pub fn set_resize_borders_visible(root: &Overlay, visible: bool) {
    let mut child = root.first_child();
    while let Some(widget) = child {
        if widget.has_css_class("card-resize-zone") {
            widget.set_visible(visible);
        }
        child = widget.next_sibling();
    }
}

fn place_zone(zone: &gtk4::Box, edge: Edge) {
    const BORDER: i32 = 8;
    const CORNER: i32 = 14;
    match edge {
        Edge::North | Edge::South => {
            zone.set_halign(Align::Fill);
            zone.set_valign(if edge == Edge::North {
                Align::Start
            } else {
                Align::End
            });
            zone.set_height_request(BORDER);
            zone.set_margin_start(CORNER);
            zone.set_margin_end(CORNER);
        }
        Edge::East | Edge::West => {
            zone.set_halign(if edge == Edge::West {
                Align::Start
            } else {
                Align::End
            });
            zone.set_valign(Align::Fill);
            zone.set_width_request(BORDER);
            zone.set_margin_top(CORNER);
            zone.set_margin_bottom(CORNER);
        }
        _ => {
            zone.set_halign(if edge.west() {
                Align::Start
            } else {
                Align::End
            });
            zone.set_valign(if edge.north() {
                Align::Start
            } else {
                Align::End
            });
            zone.set_size_request(CORNER, CORNER);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits {
            min_width: 100,
            min_height: 80,
            max_width: 500,
            max_height: 400,
            left: 10.0,
            top: 70.0,
            right: 900.0,
            bottom: 700.0,
        }
    }

    #[test]
    fn every_edge_moves_only_the_expected_sides() {
        let start = Rect {
            x: 200.0,
            y: 200.0,
            width: 300,
            height: 200,
        };
        assert_eq!(
            resized_rect(Edge::East, start, 40.0, 99.0, limits()),
            Rect {
                width: 340,
                ..start
            }
        );
        assert_eq!(
            resized_rect(Edge::South, start, 99.0, 40.0, limits()),
            Rect {
                height: 240,
                ..start
            }
        );
        assert_eq!(
            resized_rect(Edge::West, start, 40.0, 99.0, limits()),
            Rect {
                x: 240.0,
                width: 260,
                ..start
            }
        );
        assert_eq!(
            resized_rect(Edge::North, start, 99.0, 40.0, limits()),
            Rect {
                y: 240.0,
                height: 160,
                ..start
            }
        );
        assert_eq!(
            resized_rect(Edge::NorthWest, start, -50.0, -30.0, limits()),
            Rect {
                x: 150.0,
                y: 170.0,
                width: 350,
                height: 230
            }
        );
    }

    #[test]
    fn resize_clamps_to_minimum_maximum_and_screen() {
        let start = Rect {
            x: 200.0,
            y: 200.0,
            width: 300,
            height: 200,
        };
        assert_eq!(
            resized_rect(Edge::NorthWest, start, 1000.0, 1000.0, limits()),
            Rect {
                x: 400.0,
                y: 320.0,
                width: 100,
                height: 80
            }
        );
        assert_eq!(
            resized_rect(Edge::NorthWest, start, -1000.0, -1000.0, limits()),
            Rect {
                x: 10.0,
                y: 70.0,
                width: 490,
                height: 330
            }
        );
        assert_eq!(
            resized_rect(Edge::SouthEast, start, 1000.0, 1000.0, limits()),
            Rect {
                x: 200.0,
                y: 200.0,
                width: 500,
                height: 400
            }
        );
    }
}
