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
    if local.canvas.width == 0
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

/// Inverse of the fit scale: a point in the fitted canvas, back in host pixels.
///
/// The canvas widget is already centered, so card positions inside it are
/// `host * scale` with no extra offset. Returns nothing for a scale that
/// cannot be inverted.
pub fn host_origin(scale: f64, view_x: f64, view_y: f64) -> Option<(i32, i32)> {
    if !scale.is_finite() || scale <= 0.0 || !view_x.is_finite() || !view_y.is_finite() {
        return None;
    }
    Some(((view_x / scale).round() as i32, (view_y / scale).round() as i32))
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
    #[test]
    fn a_fitted_drop_lands_on_the_host_pixel_and_inside_the_canvas() {
        assert_eq!(host_origin(0.5, 50.0, 100.0), Some((100, 200)));
        assert_eq!(host_origin(1.0, 10.4, 69.6), Some((10, 70)));
        assert!(host_origin(0.0, 1.0, 1.0).is_none());
        assert_eq!(clamp_card_origin(1920, 1080, -40, 5000), (10, 1020));
        assert_eq!(clamp_card_origin(100, 80, 0, 0), (10, 70));
    }
}

#[cfg(test)]
pub fn fixture() -> WorkspaceSnapshot {
    serde_json::from_value(serde_json::json!({
        "machineId":"a".repeat(32), "epoch":"host-one", "revision":1,
        "canvas":{"x":0,"y":0,"width":1920,"height":1080,"scale":2.0,"topInset":56},
        "workspace":"/project", "homeDirectory":"/home/host", "visibleHarnesses":["shell"],
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
