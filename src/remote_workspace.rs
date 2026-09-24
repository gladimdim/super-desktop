//! Read-only remote layout presentation, independent of local sessions/state.
use crate::desktop_protocol::{DesktopCard, MachineSelection, WorkspaceSnapshot};

#[derive(Default)]
pub struct Selection {
    pub machine: MachineSelection,
    generation: u64,
}
impl Selection {
    pub fn select(&mut self, machine: MachineSelection) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.machine = machine;
        self.generation
    }
    pub fn accepts(&self, generation: u64, id: &str) -> bool {
        self.generation == generation && self.machine == MachineSelection::Remote(id.to_owned())
    }
    pub fn request(&self) -> Option<(u64, String)> {
        match &self.machine {
            MachineSelection::Local => None,
            MachineSelection::Remote(id) => Some((self.generation, id.clone())),
        }
    }
}

pub fn validate(snapshot: &WorkspaceSnapshot) -> Result<(), &'static str> {
    let local = &snapshot.local;
    if local.folders.len() > crate::desktop_protocol::MAX_FOLDERS
        || local
            .folders
            .iter()
            .any(|folder| folder.len() > crate::desktop_protocol::MAX_WORKSPACE)
        || local.canvas.width == 0
        || local.canvas.height == 0
        || local.canvas.width > 32768
        || local.canvas.height > 32768
        || local.canvas.top_inset >= local.canvas.height
        || local.cards.len() > 256
        || !local.canvas.scale.is_finite()
        || local.canvas.scale <= 0.0
    {
        return Err("invalid_remote_layout");
    }
    let mut ids = std::collections::HashSet::new();
    for card in &local.cards {
        if card.card_id.is_empty()
            || !ids.insert(&card.card_id)
            || card.layout.width == 0
            || card.layout.height == 0
            || card.layout.width > 32768
            || card.layout.height > 32768
        {
            return Err("invalid_remote_layout");
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
pub fn card_rect(card: &DesktopCard, width: u32, height: u32) -> Rect {
    let layout = &card.layout;
    if card.expanded {
        let (x, y, width, height) =
            crate::mini_terminal::expanded_rect(width as i32, height as i32);
        Rect {
            x,
            y,
            width,
            height,
        }
    } else if layout.iconified {
        Rect {
            x: layout.icon_x.unwrap_or(layout.x) as f64,
            y: layout.icon_y.unwrap_or(layout.y) as f64,
            width: 128.0,
            height: 128.0,
        }
    } else {
        Rect {
            x: layout.x as f64,
            y: layout.y as f64,
            width: layout.width as f64,
            height: layout.height as f64,
        }
    }
}
/// Logical pixels are already independent of the host's physical monitor scale.
/// A viewer never enlarges a host card: fitting is presentation only, so the
/// scale is capped at 1.0 as the documented fit rule requires.
pub fn fit(host_width: u32, host_height: u32, width: f64, height: f64) -> (f64, f64, f64) {
    let scale = (width.max(1.0) / host_width.max(1) as f64)
        .min(height.max(1.0) / host_height.max(1) as f64)
        .min(1.0);
    (
        scale,
        (width - host_width as f64 * scale) / 2.0,
        (height - host_height as f64 * scale) / 2.0,
    )
}

/// How the viewer maps the host's logical workspace onto its own canvas.
///
/// Host coordinates are logical pixels. `Fit` is the default: the whole host
/// workspace, uniformly scaled and never enlarged. `Actual` is the plan's
/// **100% + pan/scroll** mode: one host logical pixel is `zoom` viewer logical
/// pixels (1.0 is exactly 100%), and the workspace pans when it is larger than
/// the viewer. Either way the mode is presentation only: nothing here is ever
/// sent to the host except through [`ViewTransform::view_to_host`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewMode {
    Fit,
    Actual { zoom: f64 },
}

impl Default for ViewMode {
    fn default() -> Self {
        Self::Fit
    }
}

/// Zoom bounds in 100% mode. The emulator's own font bounds
/// (`card_source::MIN_FONT`/`MAX_FONT`) make anything beyond these unreadable.
pub const MIN_ZOOM: f64 = 0.25;
pub const MAX_ZOOM: f64 = 3.0;

/// A usable zoom: finite and inside the bounds, 100% for anything else.
pub fn clamp_zoom(zoom: f64) -> f64 {
    if zoom.is_finite() && zoom > 0.0 {
        zoom.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        1.0
    }
}

/// The one mapping between host logical pixels and this viewer's canvas.
///
/// Three coordinate spaces are involved:
/// * **host** — the host's logical pixels, what every command carries;
/// * **canvas** — the scaled workspace the cards live in (`host × scale`);
///   a card's own gestures work here, so pan never reaches them;
/// * **view** — the visible viewport: the canvas centered when it is smaller
///   than the viewport, and scrolled by `pan` when it is larger.
///
/// Sizes are rounded to whole pixels exactly as GTK allocates them, so the
/// transform matches the real widget geometry and input coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewTransform {
    pub scale: f64,
    pub content_width: i32,
    pub content_height: i32,
    pub view_width: i32,
    pub view_height: i32,
    /// The scroll offset, always inside `[0, max_pan]`.
    pub pan_x: f64,
    pub pan_y: f64,
}

impl ViewTransform {
    /// The transform for a host workspace of `host_width × host_height`
    /// shown in a `view_width × view_height` viewport, asking for `pan`.
    /// Fit mode never pans; 100% mode clamps the pan to the content.
    pub fn new(
        mode: ViewMode,
        host_width: u32,
        host_height: u32,
        view_width: i32,
        view_height: i32,
        pan: (f64, f64),
    ) -> Self {
        let view_width = view_width.max(1);
        let view_height = view_height.max(1);
        let scale = match mode {
            ViewMode::Fit => {
                fit(host_width, host_height, f64::from(view_width), f64::from(view_height)).0
            }
            ViewMode::Actual { zoom } => clamp_zoom(zoom),
        };
        let content_width = ((f64::from(host_width.max(1)) * scale).round() as i32).max(1);
        let content_height = ((f64::from(host_height.max(1)) * scale).round() as i32).max(1);
        let mut transform = Self {
            scale,
            content_width,
            content_height,
            view_width,
            view_height,
            pan_x: 0.0,
            pan_y: 0.0,
        };
        if !matches!(mode, ViewMode::Fit) {
            transform.set_pan(pan);
        }
        transform
    }

    /// The largest pan on each axis: zero when the content fits.
    pub fn max_pan(&self) -> (f64, f64) {
        (
            f64::from((self.content_width - self.view_width).max(0)),
            f64::from((self.content_height - self.view_height).max(0)),
        )
    }

    /// Scroll to `pan`, clamped to the content and rounded to whole pixels
    /// (what the viewport does with its own offset).
    pub fn set_pan(&mut self, pan: (f64, f64)) {
        let (max_x, max_y) = self.max_pan();
        let clamp = |value: f64, max: f64| {
            if value.is_finite() {
                value.round().clamp(0.0, max)
            } else {
                0.0
            }
        };
        self.pan_x = clamp(pan.0, max_x);
        self.pan_y = clamp(pan.1, max_y);
    }

    /// Where the canvas's origin is in the viewport.
    pub fn origin(&self) -> (f64, f64) {
        (
            f64::from((self.view_width - self.content_width).max(0) / 2) - self.pan_x,
            f64::from((self.view_height - self.content_height).max(0) / 2) - self.pan_y,
        )
    }

    /// A host point in viewport pixels.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn host_to_view(&self, x: f64, y: f64) -> (f64, f64) {
        let (ox, oy) = self.origin();
        (ox + x * self.scale, oy + y * self.scale)
    }

    /// A viewport point in host pixels: the inverse of `host_to_view`.
    pub fn view_to_host(&self, x: f64, y: f64) -> (f64, f64) {
        let (ox, oy) = self.origin();
        ((x - ox) / self.scale, (y - oy) / self.scale)
    }

    /// The transform after switching to `mode`, panned so the host point
    /// under `anchor` (viewport pixels) stays under it — a zoom around the
    /// pointer, or around the viewport's center for the toggle.
    pub fn rezoomed(
        &self,
        mode: ViewMode,
        host_width: u32,
        host_height: u32,
        anchor: (f64, f64),
    ) -> Self {
        let (hx, hy) = self.view_to_host(anchor.0, anchor.1);
        let mut next = Self::new(
            mode,
            host_width,
            host_height,
            self.view_width,
            self.view_height,
            (0.0, 0.0),
        );
        if !matches!(mode, ViewMode::Fit) {
            let centered_x = f64::from((next.view_width - next.content_width).max(0) / 2);
            let centered_y = f64::from((next.view_height - next.content_height).max(0) / 2);
            next.set_pan((
                centered_x + hx * next.scale - anchor.0,
                centered_y + hy * next.scale - anchor.1,
            ));
        }
        next
    }
}

/// Same edges the local cards use, so a remote drop cannot park a card where
/// the host would refuse to draw it.
pub fn clamp_card_origin(canvas_w: u32, canvas_h: u32, x: i32, y: i32) -> (i32, i32) {
    let max_x = (canvas_w as i32 - 80).max(10);
    let max_y = (canvas_h as i32 - 60).max(70);
    (x.clamp(10, max_x), y.clamp(70, max_y))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_results_cannot_replace_another_machine_or_a_new_visit() {
        let mut selection = Selection::default();
        assert!(selection.request().is_none());
        let old = selection.select(MachineSelection::Remote("a".into()));
        selection.select(MachineSelection::Remote("b".into()));
        assert!(!selection.accepts(old, "a"));
        let current = selection.select(MachineSelection::Remote("a".into()));
        assert!(!selection.accepts(old, "a"));
        assert!(selection.accepts(current, "a"));
        selection.select(MachineSelection::Local);
        assert!(!selection.accepts(current, "a"));
    }
    #[test]
    fn fit_preserves_aspect_and_centers_without_applying_physical_dpi() {
        assert_eq!(fit(1920, 1080, 960.0, 600.0), (0.5, 0.0, 30.0));
        assert_eq!(fit(1920, 1080, 1920.0, 1080.0), (1.0, 0.0, 0.0));
        // A larger viewer screen never scales host cards up.
        assert_eq!(fit(1920, 1080, 2560.0, 1440.0), (1.0, 320.0, 180.0));
    }
    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    #[test]
    fn fit_transform_centers_and_never_enlarges() {
        // Host larger than the viewer: half scale, centered vertically.
        let t = ViewTransform::new(ViewMode::Fit, 1920, 1080, 960, 600, (400.0, 400.0));
        assert_eq!(t.scale, 0.5);
        assert_eq!((t.content_width, t.content_height), (960, 540));
        // Fit never pans, whatever is asked for.
        assert_eq!((t.pan_x, t.pan_y), (0.0, 0.0));
        assert_eq!(t.origin(), (0.0, 30.0));
        assert_eq!(t.host_to_view(100.0, 200.0), (50.0, 130.0));
        assert_eq!(t.view_to_host(50.0, 130.0), (100.0, 200.0));
        // Host smaller than the viewer: 1:1 and centered, never scaled up.
        let t = ViewTransform::new(ViewMode::Fit, 1280, 720, 2560, 1440, (0.0, 0.0));
        assert_eq!(t.scale, 1.0);
        assert_eq!(t.origin(), (640.0, 360.0));
        assert_eq!(t.max_pan(), (0.0, 0.0));
    }

    #[test]
    fn actual_size_is_one_to_one_and_pans_inside_the_content() {
        let t = ViewTransform::new(
            ViewMode::Actual { zoom: 1.0 },
            1920,
            1080,
            960,
            600,
            (500.0, 100.0),
        );
        assert_eq!(t.scale, 1.0);
        assert_eq!((t.content_width, t.content_height), (1920, 1080));
        assert_eq!(t.max_pan(), (960.0, 480.0));
        assert_eq!((t.pan_x, t.pan_y), (500.0, 100.0));
        // A host point is exactly one viewer pixel per host pixel, shifted by
        // the pan.
        assert_eq!(t.host_to_view(700.0, 200.0), (200.0, 100.0));
        assert_eq!(t.view_to_host(200.0, 100.0), (700.0, 200.0));
        // Pan is clamped to the content on both ends, and rounded like GTK.
        let t = ViewTransform::new(
            ViewMode::Actual { zoom: 1.0 },
            1920,
            1080,
            960,
            600,
            (5000.0, -40.0),
        );
        assert_eq!((t.pan_x, t.pan_y), (960.0, 0.0));
        let t = ViewTransform::new(
            ViewMode::Actual { zoom: 1.0 },
            1920,
            1080,
            960,
            600,
            (10.4, f64::NAN),
        );
        assert_eq!((t.pan_x, t.pan_y), (10.0, 0.0));
        // A host smaller than the viewer is centered and cannot pan.
        let t = ViewTransform::new(
            ViewMode::Actual { zoom: 1.0 },
            800,
            600,
            1920,
            1080,
            (300.0, 300.0),
        );
        assert_eq!(t.max_pan(), (0.0, 0.0));
        assert_eq!(t.origin(), (560.0, 240.0));
        assert_eq!(t.host_to_view(0.0, 0.0), (560.0, 240.0));
    }

    #[test]
    fn zoom_round_trips_and_is_bounded() {
        for zoom in [0.25, 0.5, 1.0, 1.25, 2.0, 3.0] {
            for pan in [(0.0, 0.0), (137.0, 59.0), (100000.0, 100000.0)] {
                let t = ViewTransform::new(ViewMode::Actual { zoom }, 2560, 1440, 1280, 720, pan);
                for point in [(0.0, 0.0), (700.0, 200.0), (2559.0, 1439.0), (-300.0, 5000.0)] {
                    let view = t.host_to_view(point.0, point.1);
                    assert!(close(t.view_to_host(view.0, view.1), point), "{zoom} {pan:?}");
                }
            }
        }
        assert_eq!(clamp_zoom(10.0), MAX_ZOOM);
        assert_eq!(clamp_zoom(0.01), MIN_ZOOM);
        assert_eq!(clamp_zoom(f64::NAN), 1.0);
        assert_eq!(clamp_zoom(-2.0), 1.0);
        let t = ViewTransform::new(ViewMode::Actual { zoom: 9.0 }, 1920, 1080, 800, 600, (0.0, 0.0));
        assert_eq!(t.scale, MAX_ZOOM);
    }

    #[test]
    fn off_screen_host_points_map_outside_the_viewport_and_back() {
        // A card the host parked past its own edge (or saved on a larger
        // output) is still translated exactly, in both modes.
        for mode in [ViewMode::Fit, ViewMode::Actual { zoom: 1.0 }] {
            let t = ViewTransform::new(mode, 1920, 1080, 1280, 720, (300.0, 200.0));
            let view = t.host_to_view(6000.0, -500.0);
            assert!(view.0 > f64::from(t.view_width) && view.1 < 0.0);
            assert!(close(t.view_to_host(view.0, view.1), (6000.0, -500.0)));
        }
    }

    #[test]
    fn zooming_keeps_the_point_under_the_anchor() {
        let host = (1920, 1080);
        let start = ViewTransform::new(ViewMode::Actual { zoom: 1.0 }, host.0, host.1, 800, 600, (400.0, 200.0));
        let anchor = (300.0, 250.0);
        let under = start.view_to_host(anchor.0, anchor.1);
        let zoomed = start.rezoomed(ViewMode::Actual { zoom: 1.5 }, host.0, host.1, anchor);
        assert_eq!(zoomed.scale, 1.5);
        let after = zoomed.view_to_host(anchor.0, anchor.1);
        // Whole-pixel pan: within one viewer pixel of the same host point.
        assert!((after.0 - under.0).abs() * zoomed.scale <= 0.5 + 1e-9);
        assert!((after.1 - under.1).abs() * zoomed.scale <= 0.5 + 1e-9);
        // Fit → 100% around the viewport center keeps the center's host point.
        let fit = ViewTransform::new(ViewMode::Fit, host.0, host.1, 960, 600, (0.0, 0.0));
        let center = (480.0, 300.0);
        let under = fit.view_to_host(center.0, center.1);
        let actual = fit.rezoomed(ViewMode::Actual { zoom: 1.0 }, host.0, host.1, center);
        assert_eq!((actual.pan_x, actual.pan_y), (480.0, 240.0));
        assert!(close(actual.view_to_host(center.0, center.1), under));
        // Back to fit: the pan is gone and the whole workspace is visible.
        let back = actual.rezoomed(ViewMode::Fit, host.0, host.1, center);
        assert_eq!(back, fit);
        // An anchor near the edge cannot pan past the content.
        let edge = fit.rezoomed(ViewMode::Actual { zoom: 3.0 }, host.0, host.1, (960.0, 600.0));
        assert_eq!((edge.pan_x, edge.pan_y), edge.max_pan());
    }

    #[test]
    fn a_fitted_drop_lands_inside_the_canvas() {
        assert_eq!(clamp_card_origin(1920, 1080, -40, 5000), (10, 1020));
        assert_eq!(clamp_card_origin(100, 80, 0, 0), (10, 70));
    }
}

#[cfg(test)]
pub fn fixture() -> WorkspaceSnapshot {
    serde_json::from_value(serde_json::json!({
        "machineId":"a".repeat(32), "epoch":"host-one", "revision":1,
        "canvas":{"x":0,"y":0,"width":1920,"height":1080,"scale":2.0,"topInset":56},
        "workspace":"/project", "folders":["/project","/work/notes"],
        "homeDirectory":"/home/host", "visibleHarnesses":["shell"],
        "harnessTypes":[{"id":"shell","name":"Shell","available":true}],
        "cards":[{"cardId":"card-one","sessionName":"sd_term_one","agentType":"shell",
        "title":"Host shell","status":"RUNNING","sessionAlive":true,"workspace":"/project",
        "revision":1,"stackingOrder":0,"expanded":false,"terminalSize":{"columns":80,"rows":24},
        "layout":{"x":100,"y":200,"width":640,"height":480,"restoredWidth":640,"restoredHeight":480,
        "iconified":false,"iconX":32,"iconY":64,"tag":3}}]
    }))
    .unwrap()
}
#[cfg(test)]
mod layout_tests {
    use super::*;
    #[test]
    fn host_geometry_includes_icon_and_expanded_modes() {
        let mut snapshot = fixture();
        validate(&snapshot).unwrap();
        let card = &mut snapshot.local.cards[0];
        assert_eq!(
            card_rect(card, 1920, 1080),
            Rect {
                x: 100.0,
                y: 200.0,
                width: 640.0,
                height: 480.0
            }
        );
        card.layout.iconified = true;
        assert_eq!(
            card_rect(card, 1920, 1080),
            Rect {
                x: 32.0,
                y: 64.0,
                width: 128.0,
                height: 128.0
            }
        );
        card.expanded = true;
        assert_eq!(
            card_rect(card, 1920, 1080),
            Rect {
                x: 192.0,
                y: 108.0,
                width: 1536.0,
                height: 864.0
            }
        );
    }
    #[test]
    fn malformed_host_snapshots_never_reach_the_renderer() {
        let mut snapshot = fixture();
        snapshot.local.cards.push(snapshot.local.cards[0].clone());
        assert!(validate(&snapshot).is_err());
        snapshot.local.cards.pop();
        snapshot.local.canvas.width = 0;
        assert!(validate(&snapshot).is_err());
        snapshot.local.canvas.width = 1920;
        snapshot.local.cards[0].layout.width = u32::MAX;
        assert!(validate(&snapshot).is_err());
        snapshot.local.cards[0].layout.width = 640;
        snapshot.local.canvas.scale = f64::NAN;
        assert!(validate(&snapshot).is_err());
    }
}
