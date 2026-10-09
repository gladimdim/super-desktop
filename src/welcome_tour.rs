//! The welcome tour: a short, animated introduction shown once, the first time
//! a fresh install opens the overlay, and again from ⚙ Settings → Getting started guide
//! or `super-desktop tour`.
//!
//! Each slide pairs a few lines of text with a small scene: a miniature of the
//! overlay (top bar, cards, the This PC menu, a phone) built from ordinary
//! widgets, so it wears the Omarchy theme and the real harness logos. One
//! frame-clock tick poses the current scene from the time since the slide
//! opened; the poses themselves are plain functions of that time. With GTK
//! animations turned off, every scene shows one still frame instead.
use gtk4::{gdk, glib, graphene, gsk, prelude::*, Align, Orientation};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::theme::OmarchyTheme;

/// The scene's size in the panel, in logical pixels.
const STAGE: (f64, f64) = (640.0, 270.0);
/// The miniature top bar's height.
const BAR_H: f64 = 30.0;
/// The panel's width; its height follows its content.
const PANEL_WIDTH: i32 = 720;

/// Where the "Try the live demo" button goes.
pub const DEMO_URL: &str = "https://superdesktop.dmytrogladkyi.com/";

thread_local! {
    /// How ⚙ Settings → Welcome tour reaches the overlay's tour: the settings
    /// card is built lazily and knows nothing about the window.
    static REPLAY: RefCell<Option<Rc<dyn Fn()>>> = RefCell::new(None);
}

/// Register what [`replay`] opens (the overlay's tour).
pub fn set_replay(open: Rc<dyn Fn()>) {
    REPLAY.with(|slot| *slot.borrow_mut() = Some(open));
}

/// Open the tour again, if an overlay registered one.
pub fn replay() {
    if let Some(open) = REPLAY.with(|slot| slot.borrow().clone()) {
        open();
    }
}

// ---------------------------------------------------------------- timing ----

/// How far `t` is through the span `[start, start + duration]`, from 0 to 1.
fn span(t: f64, start: f64, duration: f64) -> f64 {
    if duration <= 0.0 {
        return if t >= start { 1.0 } else { 0.0 };
    }
    ((t - start) / duration).clamp(0.0, 1.0)
}

fn ease_out(p: f64) -> f64 {
    1.0 - (1.0 - p).powi(3)
}

fn ease_in(p: f64) -> f64 {
    p.powi(3)
}

fn ease_in_out(p: f64) -> f64 {
    if p < 0.5 { 4.0 * p.powi(3) } else { 1.0 - (-2.0 * p + 2.0).powi(3) / 2.0 }
}

fn lerp(a: f64, b: f64, p: f64) -> f64 {
    a + (b - a) * p
}

fn lerp2(a: (f64, f64), b: (f64, f64), p: f64) -> (f64, f64) {
    (lerp(a.0, b.0, p), lerp(a.1, b.1, p))
}

/// The first `p` (0 to 1) of `text`, as if it were being typed.
fn typed(text: &str, p: f64) -> &str {
    let count = text.chars().count();
    let shown = (count as f64 * p.clamp(0.0, 1.0)).round() as usize;
    match text.char_indices().nth(shown) {
        Some((at, _)) => &text[..at],
        None => text,
    }
}

/// Opacity that fades a looping scene in at its start and out at its end.
fn loop_fade(t: f64, period: f64) -> f64 {
    span(t, 0.0, 0.3).min(1.0 - span(t, period - 0.45, 0.45))
}

/// A click: a ring that grows and fades from `at`, or `None` outside it.
fn ripple(t: f64, at: f64) -> Option<(f64, f64)> {
    let p = span(t, at, 0.45);
    (t >= at && p < 1.0).then(|| (lerp(0.4, 1.6, ease_out(p)), 0.9 * (1.0 - p)))
}

/// The keys of a shortcut spelled `SUPER + SHIFT + Q`.
fn combo_keys(combo: &str) -> Vec<String> {
    combo.split('+').map(str::trim).filter(|k| !k.is_empty()).map(str::to_string).collect()
}

// ---------------------------------------------------------------- actors ----

/// Where and how an actor is drawn: its top-left on the stage, scale and
/// rotation about its center, and opacity.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pose {
    x: f64,
    y: f64,
    scale: f64,
    rotate: f64,
    opacity: f64,
}

impl Pose {
    fn at(x: f64, y: f64) -> Self {
        Self { x, y, scale: 1.0, rotate: 0.0, opacity: 1.0 }
    }
    fn scale(self, scale: f64) -> Self {
        Self { scale, ..self }
    }
    fn rotate(self, rotate: f64) -> Self {
        Self { rotate, ..self }
    }
    fn opacity(self, opacity: f64) -> Self {
        Self { opacity: opacity.clamp(0.0, 1.0), ..self }
    }
}

/// A widget on a scene's stage, at a fixed size.
#[derive(Clone)]
struct Actor {
    widget: gtk4::Widget,
    size: (f64, f64),
    stage: gtk4::Fixed,
}

impl Actor {
    fn put(stage: &gtk4::Fixed, widget: &impl IsA<gtk4::Widget>, size: (f64, f64)) -> Self {
        let widget = widget.as_ref().clone();
        widget.set_size_request(size.0 as i32, size.1 as i32);
        stage.put(&widget, 0.0, 0.0);
        Self { widget, size, stage: stage.clone() }
    }

    fn pose(&self, pose: Pose) {
        let visible = pose.opacity > 0.01;
        if self.widget.is_visible() != visible {
            self.widget.set_visible(visible);
        }
        if !visible {
            return;
        }
        self.widget.set_opacity(pose.opacity);
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        let transform = gsk::Transform::new()
            .translate(&graphene::Point::new(pose.x as f32 + w / 2.0, pose.y as f32 + h / 2.0))
            .rotate(pose.rotate as f32)
            .scale(pose.scale as f32, pose.scale as f32)
            .translate(&graphene::Point::new(-w / 2.0, -h / 2.0));
        self.stage.set_child_transform(&self.widget, Some(&transform));
    }

    fn hide(&self) {
        self.pose(Pose::at(0.0, 0.0).opacity(0.0));
    }
}

/// The center of `widget` on `stage`, once it has been laid out.
fn center_on(widget: &impl IsA<gtk4::Widget>, stage: &gtk4::Fixed) -> Option<(f64, f64)> {
    let widget = widget.as_ref();
    let point = graphene::Point::new(widget.width() as f32 / 2.0, widget.height() as f32 / 2.0);
    (widget.width() > 0)
        .then(|| widget.compute_point(stage, &point))
        .flatten()
        .map(|p| (p.x() as f64, p.y() as f64))
}

fn set_text(label: &gtk4::Label, text: &str) {
    if label.text().as_str() != text {
        label.set_text(text);
    }
}

fn set_class(widget: &impl IsA<gtk4::Widget>, class: &str, on: bool) {
    if widget.as_ref().has_css_class(class) != on {
        if on {
            widget.as_ref().add_css_class(class);
        } else {
            widget.as_ref().remove_css_class(class);
        }
    }
}

fn label(text: &str, class: &str) -> gtk4::Label {
    let label = gtk4::Label::new(Some(text));
    label.add_css_class(class);
    label
}

/// An agent's logo at `px`, or a dot when the logo is not installed.
fn logo(agent: &str, px: i32) -> gtk4::Widget {
    let light = crate::theme::current_theme().mode == "light";
    match crate::brand::logo_path(agent, light) {
        Some(path) => {
            let image = gtk4::Image::from_file(path);
            image.set_pixel_size(px);
            image.upcast()
        }
        None => label("●", "tour-card-title").upcast(),
    }
}

fn icon(name: &str, px: i32) -> gtk4::Image {
    let image = gtk4::Image::from_icon_name(name);
    image.set_pixel_size(px);
    image.add_css_class("tour-icon");
    image
}

/// A pointer drawn as an arrow.
fn cursor_actor(stage: &gtk4::Fixed) -> Actor {
    let area = gtk4::DrawingArea::new();
    area.set_draw_func(|_, cr, _, _| {
        cr.move_to(1.0, 1.0);
        for (x, y) in [(1.0, 17.0), (5.0, 13.5), (8.0, 20.0), (10.5, 19.0), (7.5, 12.5), (13.0, 12.5)] {
            cr.line_to(x, y);
        }
        cr.close_path();
        cr.set_source_rgb(1.0, 1.0, 1.0);
        let _ = cr.fill_preserve();
        cr.set_source_rgb(0.05, 0.05, 0.08);
        cr.set_line_width(1.2);
        let _ = cr.stroke();
    });
    Actor::put(stage, &area, (16.0, 22.0))
}

fn ripple_actor(stage: &gtk4::Fixed) -> Actor {
    let ring = gtk4::Box::new(Orientation::Vertical, 0);
    ring.add_css_class("tour-ripple");
    Actor::put(stage, &ring, (34.0, 34.0))
}

/// Pose the click ring centered on `at`, if a click is under way.
fn pose_ripple(ring: &Actor, t: f64, click: f64, at: (f64, f64)) -> bool {
    match ripple(t, click) {
        Some((scale, opacity)) => {
            ring.pose(Pose::at(at.0 - ring.size.0 / 2.0, at.1 - ring.size.1 / 2.0).scale(scale).opacity(opacity));
            true
        }
        None => false,
    }
}

/// The miniature top bar.
struct Bar {
    actor: Actor,
    pc: gtk4::Label,
    launchers: Vec<gtk4::Box>,
    gear: gtk4::Image,
    hide: gtk4::Image,
}

fn bar(stage: &gtk4::Fixed, width: f64, launchers: &[(&str, &str)]) -> Bar {
    bar_with(stage, width, launchers, true)
}

/// [`bar`], optionally without the folder field (for a narrow bar).
fn bar_with(stage: &gtk4::Fixed, width: f64, launchers: &[(&str, &str)], folder: bool) -> Bar {
    let root = gtk4::Box::new(Orientation::Horizontal, 6);
    root.add_css_class("tour-bar");
    let pc = label("This PC ▾", "tour-pc");
    pc.set_valign(Align::Center);
    root.append(&pc);
    let brand = label("⚡ SUPER DESKTOP", "tour-brand");
    root.append(&brand);
    if folder {
        let folder = label("~/code/app", "tour-folder");
        folder.set_valign(Align::Center);
        root.append(&folder);
    }
    let mut items = Vec::new();
    for (agent, name) in launchers {
        let item = gtk4::Box::new(Orientation::Horizontal, 4);
        item.add_css_class("tour-launch");
        item.set_valign(Align::Center);
        if agent.starts_with("custom:") {
            item.append(&label(&agent["custom:".len()..], "tour-launch-emoji"));
        } else {
            item.append(&logo(agent, 12));
        }
        item.append(&label(name, "tour-launch-name"));
        root.append(&item);
        items.push(item);
    }
    let spacer = gtk4::Box::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    root.append(&spacer);
    root.append(&icon("sd-arrange-symbolic", 12));
    let gear = icon("sd-gears-symbolic", 12);
    root.append(&gear);
    let hide = icon("sd-hide-symbolic", 12);
    root.append(&hide);
    let actor = Actor::put(stage, &root, (width, BAR_H));
    Bar { actor, pc, launchers: items, gear, hide }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Status {
    None,
    Idle,
    Working,
    Finished,
}

/// A miniature terminal card.
struct Card {
    actor: Actor,
    status: gtk4::Label,
    body: gtk4::Label,
}

impl Card {
    fn set_status(&self, status: Status) {
        let (text, class) = match status {
            Status::None => ("", ""),
            Status::Idle => ("idle", "idle"),
            Status::Working => ("● working", "working"),
            Status::Finished => ("✓ finished", "finished"),
        };
        set_text(&self.status, text);
        self.status.set_visible(status != Status::None);
        for name in ["idle", "working", "finished"] {
            set_class(&self.status, name, name == class);
        }
    }

    fn set_body(&self, text: &str) {
        set_text(&self.body, text);
    }
}

fn card(stage: &gtk4::Fixed, agent: &str, title: &str, size: (f64, f64)) -> Card {
    let root = gtk4::Box::new(Orientation::Vertical, 0);
    root.add_css_class("tour-card");
    root.set_overflow(gtk4::Overflow::Hidden);
    let head = gtk4::Box::new(Orientation::Horizontal, 6);
    head.add_css_class("tour-card-head");
    head.append(&logo(agent, 12));
    let name = label(title, "tour-card-title");
    name.set_hexpand(true);
    name.set_xalign(0.0);
    name.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    head.append(&name);
    let status = label("", "tour-status");
    status.set_visible(false);
    head.append(&status);
    root.append(&head);
    let body = label("", "tour-card-body");
    body.set_xalign(0.0);
    body.set_yalign(0.0);
    body.set_valign(Align::Start);
    body.set_wrap(true);
    body.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    root.append(&body);
    Card { actor: Actor::put(stage, &root, size), status, body }
}

/// Key caps for a shortcut, in a row.
fn keycaps(keys: &[String]) -> (gtk4::Box, Vec<gtk4::Label>) {
    let row = gtk4::Box::new(Orientation::Horizontal, 6);
    // Centered in the full-width box it is put in.
    row.set_hexpand(true);
    row.set_halign(Align::Center);
    let mut caps = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        if i > 0 {
            row.append(&label("+", "tour-key-plus"));
        }
        let cap = label(key, "tour-key");
        row.append(&cap);
        caps.push(cap);
    }
    (row, caps)
}

// ---------------------------------------------------------------- scenes ----

/// A slide's animated picture.
struct Scene {
    /// What the panel shows: the stage, clipped to [`STAGE`].
    widget: gtk4::Widget,
    /// Seconds before the animation starts over.
    period: f64,
    /// The time shown when animations are off.
    still: f64,
    animate: Box<dyn Fn(f64)>,
}

/// A clipped stage of [`STAGE`] size. Its actors may move off it without
/// growing it: the fixed is an overlay child, which the overlay does not
/// measure.
fn stage() -> (gtk4::Overlay, gtk4::Fixed) {
    let frame = gtk4::Overlay::new();
    frame.add_css_class("tour-stage");
    frame.set_overflow(gtk4::Overflow::Hidden);
    frame.set_halign(Align::Center);
    let base = gtk4::Box::new(Orientation::Vertical, 0);
    base.set_size_request(STAGE.0 as i32, STAGE.1 as i32);
    frame.set_child(Some(&base));
    let fixed = gtk4::Fixed::new();
    fixed.set_can_target(false);
    frame.add_overlay(&fixed);
    (frame, fixed)
}

fn scene(frame: gtk4::Overlay, period: f64, still: f64, animate: impl Fn(f64) + 'static) -> Scene {
    Scene { widget: frame.upcast(), period, still, animate: Box::new(animate) }
}

const AGENTS: [(&str, &str); 4] = [("claude", "Claude"), ("codex", "Codex"), ("opencode", "OpenCode"), ("shell", "Shell")];

/// 1. Loose terminal windows gather into one workspace.
fn scene_welcome() -> Scene {
    let (frame, fixed) = stage();
    let period = 7.0;
    let cards: Vec<Card> = [
        ("claude", "Claude Code · app", "> refactor the auth module\n● Reading src/auth.rs…"),
        ("codex", "Codex · api", "> write tests for the parser\n✓ 14 tests passing"),
        ("opencode", "OpenCode · docs", "> explain this stack trace\n● Thinking…"),
        ("shell", "Shell · app", "$ npm run dev\n  ready on http://localhost:5173"),
    ]
    .into_iter()
    .map(|(agent, title, body)| {
        let card = card(&fixed, agent, title, (300.0, 104.0));
        card.set_body(body);
        card
    })
    .collect();
    let bar = bar(&fixed, STAGE.0, &AGENTS);
    let scattered = [(-20.0, 120.0, -9.0), (380.0, 150.0, 7.0), (110.0, -12.0, 5.0), (300.0, 30.0, -6.0)];
    let grid = [(14.0, 44.0), (326.0, 44.0), (14.0, 158.0), (326.0, 158.0)];
    let statuses = [Status::Working, Status::Finished, Status::Working, Status::Idle];
    scene(frame, period, 3.5, move |t| {
        let fade = loop_fade(t, period);
        let gather = ease_out(span(t, 0.9, 1.1));
        for (i, card) in cards.iter().enumerate() {
            let (sx, sy, rot) = scattered[i];
            let bob = (t * 2.2 + i as f64).sin() * 4.0 * (1.0 - gather);
            let (x, y) = lerp2((sx, sy + bob), grid[i], gather);
            card.actor.pose(Pose::at(x, y).rotate(rot * (1.0 - gather)).scale(lerp(0.86, 1.0, gather)).opacity(fade));
            card.set_status(if t > 2.2 + i as f64 * 0.15 { statuses[i] } else { Status::None });
        }
        let drop = ease_out(span(t, 0.9, 0.7));
        bar.actor.pose(Pose::at(0.0, lerp(-BAR_H - 4.0, 0.0, drop)).opacity(fade.min(drop)));
    })
}

/// 2. A click on a launcher starts an agent in a new card.
fn scene_launch() -> Scene {
    let (frame, fixed) = stage();
    let period = 7.5;
    let shell = card(&fixed, "shell", "Shell · app", (290.0, 150.0));
    shell.set_body("$ git status\nOn branch main\nnothing to commit");
    shell.set_status(Status::Idle);
    let claude = card(&fixed, "claude", "Claude Code · app", (300.0, 150.0));
    let bar = bar(&fixed, STAGE.0, &AGENTS);
    let ring = ripple_actor(&fixed);
    let cursor = cursor_actor(&fixed);
    let prompt = "> add a dark mode toggle to settings";
    scene(frame, period, 5.8, move |t| {
        let fade = loop_fade(t, period);
        bar.actor.pose(Pose::at(0.0, 0.0));
        shell.actor.pose(Pose::at(336.0, 52.0).opacity(fade));
        let launcher = center_on(&bar.launchers[0], &fixed).unwrap_or((250.0, 15.0));
        let click = 1.3;
        set_class(&bar.launchers[0], "hot", (1.1..1.9).contains(&t));
        if !pose_ripple(&ring, t, click, launcher) {
            ring.hide();
        }
        // The card grows out of the launcher.
        let grow = ease_out(span(t, 1.5, 0.6));
        let target = (20.0, 52.0);
        let from = (launcher.0 - claude.actor.size.0 / 2.0, launcher.1 - claude.actor.size.1 / 2.0);
        let (x, y) = lerp2(from, target, grow);
        claude.actor.pose(Pose::at(x, y).scale(lerp(0.08, 1.0, grow)).opacity(fade.min(span(t, 1.5, 0.2))));
        let typing = span(t, 2.6, 1.4);
        let mut body = typed(prompt, typing).to_string();
        if t > 4.5 {
            body.push_str("\n● Editing src/settings.rs…");
        }
        if t > 6.0 {
            body.push_str("\n✓ Done: 3 files changed");
        }
        claude.set_body(&body);
        claude.set_status(if t > 6.0 { Status::Finished } else if t > 4.1 { Status::Working } else if grow >= 1.0 { Status::Idle } else { Status::None });
        // The pointer: to the launcher, then into the new card.
        let start = (560.0, 240.0);
        let inside = (150.0, 150.0);
        let at = if t < 2.1 {
            lerp2(start, launcher, ease_in_out(span(t, 0.3, 1.0)))
        } else {
            lerp2(launcher, inside, ease_in_out(span(t, 2.1, 0.5)))
        };
        cursor.pose(Pose::at(at.0 - 1.0, at.1 - 1.0).opacity(fade));
    })
}

/// 3. ⚙ Settings adds a harness of your own to the bar.
fn scene_add() -> Scene {
    let (frame, fixed) = stage();
    let period = 8.0;
    let bar = bar(&fixed, STAGE.0, &[("claude", "Claude"), ("codex", "Codex"), ("custom:🤖", "my-agent")]);
    let custom = bar.launchers[2].clone();
    let panel = gtk4::Box::new(Orientation::Vertical, 0);
    panel.add_css_class("tour-card");
    let head = gtk4::Box::new(Orientation::Horizontal, 6);
    head.add_css_class("tour-card-head");
    head.append(&label("⚙", "tour-card-title"));
    let title = label("Settings · Harness launchers", "tour-card-title");
    title.set_xalign(0.0);
    head.append(&title);
    panel.append(&head);
    let rows = gtk4::Box::new(Orientation::Vertical, 4);
    rows.add_css_class("tour-panel-rows");
    for (agent, name, on) in [("claude", "Claude Code", true), ("codex", "Codex", true), ("opencode", "OpenCode", false)] {
        let row = gtk4::Box::new(Orientation::Horizontal, 6);
        row.add_css_class("tour-row");
        row.append(&logo(agent, 12));
        let name = label(name, "tour-row-name");
        name.set_hexpand(true);
        name.set_xalign(0.0);
        row.append(&name);
        let toggle = label(if on { "ON" } else { "OFF" }, "tour-toggle");
        set_class(&toggle, "off", !on);
        row.append(&toggle);
        rows.append(&row);
    }
    let add = gtk4::Box::new(Orientation::Horizontal, 6);
    add.add_css_class("tour-row");
    add.add_css_class("tour-add-row");
    add.append(&label("＋  Add a harness", "tour-row-name"));
    rows.append(&add);
    let form = gtk4::Box::new(Orientation::Horizontal, 6);
    let field = label("", "tour-field");
    field.set_hexpand(true);
    field.set_xalign(0.0);
    form.append(&field);
    let confirm = label("Add", "tour-button");
    form.append(&confirm);
    rows.append(&form);
    panel.append(&rows);
    let panel = Actor::put(&fixed, &panel, (290.0, 178.0));
    let ring = ripple_actor(&fixed);
    let cursor = cursor_actor(&fixed);
    let command = "~/bin/my-agent --fast";
    scene(frame, period, 4.0, move |t| {
        let fade = loop_fade(t, period);
        let shown = t >= 5.2;
        if custom.is_visible() != shown {
            custom.set_visible(shown);
        }
        custom.set_opacity(span(t, 5.2, 0.4));
        set_class(&custom, "hot", (5.2..7.2).contains(&t));
        bar.actor.pose(Pose::at(0.0, 0.0));
        let gear = center_on(&bar.gear, &fixed).unwrap_or((598.0, 15.0));
        let open = ease_out(span(t, 1.2, 0.35)) * (1.0 - span(t, 4.8, 0.4));
        panel.pose(Pose::at(334.0, 44.0).scale(lerp(0.94, 1.0, open)).opacity(open.min(fade)));
        set_class(&add, "hot", (2.4..2.9).contains(&t));
        form.set_visible(t >= 2.6);
        set_text(&field, typed(command, span(t, 2.8, 1.2)));
        set_class(&confirm, "hot", (4.5..4.9).contains(&t));
        let add_at = center_on(&add, &fixed).unwrap_or((470.0, 150.0));
        let confirm_at = center_on(&confirm, &fixed).unwrap_or((600.0, 180.0));
        let custom_at = center_on(&custom, &fixed).unwrap_or((270.0, 15.0));
        let clicks = [(1.1, gear), (2.5, add_at), (4.6, confirm_at), (6.1, custom_at)];
        if !clicks.iter().any(|(at, point)| pose_ripple(&ring, t, *at, *point)) {
            ring.hide();
        }
        let path = [
            (0.0, (480.0, 250.0)),
            (1.1, gear),
            (2.5, add_at),
            (4.6, confirm_at),
            (6.1, custom_at),
        ];
        let at = follow(&path, t, 0.7);
        cursor.pose(Pose::at(at.0 - 1.0, at.1 - 1.0).opacity(fade));
    })
}

/// A pointer that arrives at each `(time, point)` of `path`, travelling for
/// `travel` seconds before each arrival.
fn follow(path: &[(f64, (f64, f64))], t: f64, travel: f64) -> (f64, f64) {
    let mut at = path[0].1;
    for (arrive, point) in &path[1..] {
        at = lerp2(at, *point, ease_in_out(span(t, arrive - travel, travel)));
    }
    at
}

/// 4. The shortcut hides the workspace; the agents keep working.
fn scene_hide(combo: &Rc<RefCell<String>>) -> (Scene, Rc<dyn Fn(&str)>) {
    let (frame, fixed) = stage();
    let period = 8.0;
    // What is under the overlay: an app in a browser.
    let browser = gtk4::Box::new(Orientation::Vertical, 8);
    browser.add_css_class("tour-browser");
    let url = label("localhost:5173 — Demo App", "tour-browser-url");
    url.set_xalign(0.0);
    browser.append(&url);
    for width in [360, 300, 330, 180] {
        let line = gtk4::Box::new(Orientation::Horizontal, 0);
        line.add_css_class("tour-line");
        line.set_size_request(width, 8);
        line.set_halign(Align::Start);
        browser.append(&line);
    }
    let browser = Actor::put(&fixed, &browser, (600.0, 196.0));
    let dim = gtk4::Box::new(Orientation::Vertical, 0);
    dim.add_css_class("tour-dim");
    let dim = Actor::put(&fixed, &dim, STAGE);
    let claude = card(&fixed, "claude", "Claude Code · app", (300.0, 120.0));
    let codex = card(&fixed, "codex", "Codex · api", (300.0, 120.0));
    codex.set_body("> write tests for the parser\n● Running cargo test…");
    let bar = bar(&fixed, STAGE.0, &AGENTS[..3]);
    let note = label("Hidden. Your agents keep working.", "tour-caption");
    let note = Actor::put(&fixed, &note, (260.0, 26.0));
    let keys_box = gtk4::Box::new(Orientation::Horizontal, 0);
    keys_box.set_halign(Align::Center);
    let keys = Actor::put(&fixed, &keys_box, (STAGE.0, 34.0));
    let caps: Rc<RefCell<Vec<gtk4::Label>>> = Rc::new(RefCell::new(Vec::new()));
    let paint_keys: Rc<dyn Fn(&str)> = Rc::new({
        let caps = Rc::clone(&caps);
        move |combo: &str| {
            while let Some(child) = keys_box.first_child() {
                keys_box.remove(&child);
            }
            let (row, labels) = keycaps(&combo_keys(combo));
            keys_box.append(&row);
            *caps.borrow_mut() = labels;
        }
    });
    paint_keys(&combo.borrow());
    let scene = scene(frame, period, 2.6, move |t| {
        let fade = loop_fade(t, period);
        browser.pose(Pose::at(20.0, 14.0).opacity(fade));
        let pressed = (0.7..1.1).contains(&t) || (3.8..4.2).contains(&t);
        for cap in caps.borrow().iter() {
            set_class(cap, "pressed", pressed);
        }
        keys.pose(Pose::at(0.0, 226.0).opacity(fade));
        // 0 = on screen, 1 = hidden above it.
        let away = ease_in(span(t, 1.0, 0.45)) * (1.0 - ease_out(span(t, 4.1, 0.5)));
        let lift = -STAGE.1 * away;
        let shown = (1.0 - away).min(fade);
        dim.pose(Pose::at(0.0, 0.0).opacity(shown));
        bar.actor.pose(Pose::at(0.0, lift).opacity(shown));
        claude.actor.pose(Pose::at(14.0, 50.0 + lift).opacity(shown));
        codex.actor.pose(Pose::at(326.0, 50.0 + lift).opacity(shown));
        let done = t > 3.0;
        claude.set_body(if done {
            "> fix the flaky login test\n✓ Fixed: waits for the session cookie"
        } else {
            "> fix the flaky login test\n● Running the test 20 times…"
        });
        claude.set_status(if done { Status::Finished } else { Status::Working });
        codex.set_status(Status::Working);
        let caption = span(t, 1.6, 0.3) * (1.0 - span(t, 3.6, 0.3));
        note.pose(Pose::at((STAGE.0 - 260.0) / 2.0, 186.0).opacity(caption.min(fade)));
        // Hide in the bar does the same as the shortcut.
        set_class(&bar.hide, "hot", (0.7..1.1).contains(&t));
    });
    (scene, paint_keys)
}

/// 5. The phone sends a prompt to a card on this PC and hears when it is done.
fn scene_phone() -> Scene {
    let (frame, fixed) = stage();
    let period = 8.0;
    let monitor = gtk4::Box::new(Orientation::Vertical, 0);
    monitor.add_css_class("tour-monitor");
    let monitor = Actor::put(&fixed, &monitor, (420.0, 236.0));
    let bar = bar_with(&fixed, 404.0, &AGENTS[..2], false);
    let claude = card(&fixed, "claude", "Claude Code · app", (190.0, 150.0));
    claude.set_body("> refactor the auth module\n● Reading src/auth.rs…");
    let codex = card(&fixed, "codex", "Codex · api", (190.0, 150.0));
    // The phone.
    let phone = gtk4::Box::new(Orientation::Vertical, 6);
    phone.add_css_class("tour-phone");
    let top = label("This PC · 2 sessions", "tour-phone-title");
    top.set_xalign(0.0);
    phone.append(&top);
    let mut rows = Vec::new();
    for (agent, name) in [("claude", "Claude Code"), ("codex", "Codex")] {
        let row = gtk4::Box::new(Orientation::Horizontal, 6);
        row.add_css_class("tour-phone-row");
        row.append(&logo(agent, 12));
        let words = gtk4::Box::new(Orientation::Vertical, 1);
        words.set_hexpand(true);
        let title = label(name, "tour-row-name");
        title.set_xalign(0.0);
        words.append(&title);
        let state = label("", "tour-phone-state");
        state.set_xalign(0.0);
        words.append(&state);
        row.append(&words);
        phone.append(&row);
        rows.push((row, state));
    }
    let spacer = gtk4::Box::new(Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    phone.append(&spacer);
    let input = label("", "tour-field");
    input.set_xalign(0.0);
    input.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    phone.append(&input);
    let phone = Actor::put(&fixed, &phone, (150.0, 246.0));
    let banner = label("✓ Codex finished", "tour-banner");
    let banner = Actor::put(&fixed, &banner, (136.0, 30.0));
    let pill = label("run the tests", "tour-pill");
    let pill = Actor::put(&fixed, &pill, (96.0, 22.0));
    let ring = ripple_actor(&fixed);
    let prompt = "run the tests";
    scene(frame, period, 6.0, move |t| {
        let fade = loop_fade(t, period);
        monitor.pose(Pose::at(10.0, 10.0).opacity(fade));
        bar.actor.pose(Pose::at(18.0, 18.0).opacity(fade));
        claude.actor.pose(Pose::at(26.0, 60.0).opacity(fade));
        claude.set_status(Status::Working);
        codex.actor.pose(Pose::at(226.0, 60.0).opacity(fade));
        let slide = ease_out(span(t, 0.1, 0.6));
        let phone_x = lerp(STAGE.0 + 10.0, 470.0, slide);
        phone.pose(Pose::at(phone_x, 12.0).opacity(fade));
        let sent = t >= 3.5;
        let done = t >= 5.4;
        codex.set_body(if sent { "> run the tests" } else { "" });
        codex.set_status(if done { Status::Finished } else if sent { Status::Working } else { Status::Idle });
        if t > 5.6 {
            let body = "> run the tests\n✓ 42 passed in 3.1s";
            codex.set_body(body);
        }
        set_text(&rows[0].1, "● working");
        set_text(&rows[1].1, if done { "✓ finished" } else if sent { "● working" } else { "idle" });
        set_class(&rows[1].0, "hot", (0.9..3.6).contains(&t));
        set_text(&input, typed(prompt, span(t, 1.4, 1.2)).trim_end());
        if t >= 2.8 {
            set_text(&input, "");
        }
        let row_at = center_on(&rows[1].0, &fixed).unwrap_or((545.0, 100.0));
        let input_at = center_on(&input, &fixed).unwrap_or((545.0, 238.0));
        if !(pose_ripple(&ring, t, 1.0, row_at) || pose_ripple(&ring, t, 2.7, input_at)) {
            ring.hide();
        }
        // The prompt flies from the phone to the card on the PC.
        let fly = span(t, 2.8, 0.7);
        if (2.8..3.5).contains(&t) {
            let target = (226.0 + 95.0, 60.0 + 75.0);
            let (x, y) = lerp2(input_at, target, ease_in_out(fly));
            let arc = -60.0 * (std::f64::consts::PI * fly).sin();
            pill.pose(Pose::at(x - 48.0, y - 11.0 + arc).scale(lerp(1.0, 0.7, fly)).opacity(1.0 - span(t, 3.3, 0.2)));
        } else {
            pill.hide();
        }
        let drop = ease_out(span(t, 5.5, 0.35));
        banner.pose(Pose::at(phone_x + 7.0, lerp(0.0, 20.0, drop)).opacity(drop.min(fade)));
    })
}

/// 6. The This PC menu opens another PC's workspace.
fn scene_remote() -> Scene {
    let (frame, fixed) = stage();
    let period = 8.0;
    let border = gtk4::Box::new(Orientation::Vertical, 0);
    border.add_css_class("tour-remote-frame");
    let border = Actor::put(&fixed, &border, (STAGE.0 - 8.0, STAGE.1 - BAR_H - 8.0));
    let local = [
        card(&fixed, "claude", "Claude Code · app", (296.0, 150.0)),
        card(&fixed, "shell", "Shell · app", (296.0, 150.0)),
    ];
    local[0].set_body("> refactor the auth module\n● Reading src/auth.rs…");
    local[0].set_status(Status::Working);
    local[1].set_body("$ git status\nOn branch main");
    local[1].set_status(Status::Idle);
    let remote = [
        card(&fixed, "codex", "Codex · studio-pc", (296.0, 150.0)),
        card(&fixed, "opencode", "OpenCode · studio-pc", (296.0, 150.0)),
    ];
    for card in &remote {
        card.actor.widget.add_css_class("remote");
    }
    remote[1].set_body("> profile the render loop\n✓ 2 hot spots found");
    remote[1].set_status(Status::Finished);
    let bar = bar(&fixed, STAGE.0, &AGENTS[..3]);
    let menu = gtk4::Box::new(Orientation::Vertical, 2);
    menu.add_css_class("tour-menu");
    let mut items = Vec::new();
    for (name, note) in [("This PC", "laptop"), ("studio-pc", "paired"), ("＋ Add a PC", "")] {
        let item = gtk4::Box::new(Orientation::Horizontal, 6);
        item.add_css_class("tour-menu-item");
        let title = label(name, "tour-row-name");
        title.set_xalign(0.0);
        title.set_hexpand(true);
        item.append(&title);
        if !note.is_empty() {
            item.append(&label(note, "tour-menu-note"));
        }
        menu.append(&item);
        items.push(item);
    }
    let menu = Actor::put(&fixed, &menu, (180.0, 92.0));
    let ring = ripple_actor(&fixed);
    let cursor = cursor_actor(&fixed);
    let prompt = "> deploy to staging";
    scene(frame, period, 5.4, move |t| {
        let fade = loop_fade(t, period);
        bar.actor.pose(Pose::at(0.0, 0.0));
        let switched = t >= 2.4;
        set_text(&bar.pc, if switched { "studio-pc ▾" } else { "This PC ▾" });
        set_class(&bar.pc, "remote", switched);
        let open = span(t, 1.2, 0.2) * (1.0 - span(t, 2.3, 0.2));
        menu.pose(Pose::at(8.0, BAR_H + 2.0).scale(lerp(0.96, 1.0, open)).opacity(open));
        set_class(&items[0], "hot", t < 1.9);
        set_class(&items[1], "hot", t >= 1.9);
        let swap = ease_in_out(span(t, 2.4, 0.6));
        let slots = [(16.0, 52.0), (328.0, 52.0)];
        for (i, card) in local.iter().enumerate() {
            card.actor.pose(Pose::at(slots[i].0 - STAGE.0 * swap, slots[i].1).opacity(fade));
        }
        for (i, card) in remote.iter().enumerate() {
            card.actor.pose(Pose::at(slots[i].0 + STAGE.0 * (1.0 - swap), slots[i].1).opacity(fade));
        }
        border.pose(Pose::at(4.0, BAR_H + 4.0).opacity(span(t, 2.7, 0.3).min(fade)));
        let mut body = typed(prompt, span(t, 3.7, 1.3)).to_string();
        if t > 5.2 {
            body.push_str("\n● Building the release…");
        }
        remote[0].set_body(&body);
        remote[0].set_status(if t > 5.2 { Status::Working } else { Status::Idle });
        let chip = center_on(&bar.pc, &fixed).unwrap_or((40.0, 15.0));
        let studio = center_on(&items[1], &fixed).unwrap_or((90.0, 62.0));
        let inside = (150.0, 140.0);
        if !(pose_ripple(&ring, t, 1.1, chip) || pose_ripple(&ring, t, 2.1, studio)) {
            ring.hide();
        }
        let at = follow(&[(0.0, (420.0, 230.0)), (1.1, chip), (2.1, studio), (3.5, inside)], t, 0.6);
        cursor.pose(Pose::at(at.0 - 1.0, at.1 - 1.0).opacity(fade));
    })
}

/// 7. One shortcut brings it all in and puts it away.
fn scene_done(combo: &Rc<RefCell<String>>) -> (Scene, Rc<dyn Fn(&str)>) {
    let (frame, fixed) = stage();
    let period = 6.0;
    let cards: Vec<Card> = [("claude", "Claude Code"), ("codex", "Codex"), ("opencode", "OpenCode")]
        .into_iter()
        .map(|(agent, title)| card(&fixed, agent, title, (190.0, 84.0)))
        .collect();
    cards[0].set_body("✓ finished the refactor");
    cards[1].set_body("● running the tests");
    cards[2].set_body("> explain this diff");
    let bar = bar(&fixed, STAGE.0, &AGENTS);
    let keys_box = gtk4::Box::new(Orientation::Horizontal, 0);
    keys_box.add_css_class("tour-keys-large");
    keys_box.set_halign(Align::Center);
    let keys = Actor::put(&fixed, &keys_box, (STAGE.0, 44.0));
    let caps: Rc<RefCell<Vec<gtk4::Label>>> = Rc::new(RefCell::new(Vec::new()));
    let paint_keys: Rc<dyn Fn(&str)> = Rc::new({
        let caps = Rc::clone(&caps);
        move |combo: &str| {
            while let Some(child) = keys_box.first_child() {
                keys_box.remove(&child);
            }
            let (row, labels) = keycaps(&combo_keys(combo));
            keys_box.append(&row);
            *caps.borrow_mut() = labels;
        }
    });
    paint_keys(&combo.borrow());
    let statuses = [Status::Finished, Status::Working, Status::Idle];
    let scene = scene(frame, period, 2.4, move |t| {
        for (i, cap) in caps.borrow().iter().enumerate() {
            let down = 0.4 + i as f64 * 0.12;
            set_class(cap, "pressed", (down..1.3).contains(&t) || (4.0..4.4).contains(&t));
        }
        // In after the first press, away after the second.
        let shown = ease_out(span(t, 1.0, 0.5)) * (1.0 - ease_in(span(t, 4.3, 0.4)));
        bar.actor.pose(Pose::at(0.0, lerp(-BAR_H - 4.0, 0.0, shown)).opacity(shown));
        // Centered while the workspace is away, under the cards once it is in.
        keys.pose(Pose::at(0.0, lerp(110.0, 196.0, shown)));
        for (i, card) in cards.iter().enumerate() {
            let pop = ease_out(span(t, 1.1 + i as f64 * 0.12, 0.45)) * (1.0 - ease_in(span(t, 4.3, 0.4)));
            card.actor.pose(Pose::at(14.0 + i as f64 * 206.0, 46.0).scale(lerp(0.7, 1.0, pop)).opacity(pop));
            card.set_status(statuses[i]);
        }
    });
    (scene, paint_keys)
}

// ----------------------------------------------------------------- slides ---

/// A slide's words. `{combo}` stands for the show / hide shortcut.
struct Words {
    eyebrow: &'static str,
    title: &'static str,
    body: &'static str,
}

const WORDS: [Words; 7] = [
    Words {
        eyebrow: "WELCOME",
        title: "All your terminals and agents, in one place",
        body: "SUPER DESKTOP is the home for every terminal and AI coding agent you run. Start them here instead of in separate windows: they stay organized, keep running in the background, and follow you to your phone and your other PCs.",
    },
    Words {
        eyebrow: "START ANY HARNESS",
        title: "Click a logo. An agent starts.",
        body: "Every coding agent installed on this PC gets a launcher in the top bar. Click one and it opens in its own live terminal card, in the folder shown next to ⚡ SUPER DESKTOP. Type right into the card. Shell gives you a plain terminal.",
    },
    Words {
        eyebrow: "ADD YOUR OWN",
        title: "Bring any harness",
        body: "Open ⚙ Settings → Harness launchers to choose which agents sit in the top bar. Missing one? Add a harness with its executable and arguments, and it gets a launcher like the rest.",
    },
    Words {
        eyebrow: "HIDE ANYTIME",
        title: "Gone in a blink. Still working.",
        body: "Press {combo}, or click Hide in the top right, to put everything away. Your agents keep running in the background. Press it again and every card is right where you left it.",
    },
    Words {
        eyebrow: "ON THE GO",
        title: "Your agents, in your pocket",
        body: "Get the SUPER DESKTOP app for Android and pair it in ⚙ Settings → Connections → Add a device. Every session on this PC shows up on your phone: type a prompt there and it runs here, and get an alert when an agent finishes.",
    },
    Words {
        eyebrow: "YOUR OTHER PCS",
        title: "Reach every PC from here",
        body: "Pair your computers once in ⚙ Settings → Connections. Then pick another PC from the This PC menu in the top left to open its agents here. Type into a remote card and it runs on that machine.",
    },
    Words {
        eyebrow: "YOU'RE ALL SET",
        title: "One shortcut away",
        body: "Press {combo} any time to show or hide SUPER DESKTOP. Esc hides it too. Open this guide again any time from ⚙ Settings → Getting started guide.",
    },
];

pub const STEPS: usize = WORDS.len();

fn fill_combo(text: &str, combo: &str) -> String {
    text.replace("{combo}", combo)
}

/// What the tour asks of the overlay.
pub struct TourHooks {
    /// The top bar's current height: the tour leaves the bar uncovered.
    pub top: Rc<dyn Fn() -> i32>,
    /// Put the overlay away (after opening the website).
    pub hide_overlay: Rc<dyn Fn()>,
}

pub struct WelcomeTour {
    /// A full-size layer that dims the workspace under the bar and centers
    /// the tour card. Hidden while the tour is closed.
    pub widget: gtk4::Box,
    panel: gtk4::Box,
    stack: gtk4::Stack,
    scenes: Vec<Scene>,
    bodies: Vec<gtk4::Label>,
    paint_keys: Vec<Rc<dyn Fn(&str)>>,
    combo: Rc<RefCell<String>>,
    dots: Vec<gtk4::Button>,
    step_label: gtk4::Label,
    back: gtk4::Button,
    next: gtk4::Button,
    step: Cell<usize>,
    /// Frame time (seconds) the current slide's animation started at.
    started: Cell<Option<f64>>,
    tick: RefCell<Option<gtk4::TickCallbackId>>,
    hooks: TourHooks,
}

fn animations_enabled() -> bool {
    gtk4::Settings::default().is_none_or(|settings| settings.is_gtk_enable_animations())
}

impl WelcomeTour {
    pub fn new(combo: &str, hooks: TourHooks) -> Rc<Self> {
        let combo = Rc::new(RefCell::new(combo.to_string()));
        let widget = gtk4::Box::new(Orientation::Vertical, 0);
        widget.add_css_class("tour-scrim");
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.set_visible(false);

        let panel = gtk4::Box::new(Orientation::Vertical, 0);
        panel.add_css_class("mini-terminal");
        panel.add_css_class("harness-panel");
        panel.add_css_class("tour-panel");
        panel.set_size_request(PANEL_WIDTH, -1);
        panel.set_halign(Align::Center);
        panel.set_valign(Align::Center);
        panel.set_vexpand(true);
        widget.append(&panel);

        let header = gtk4::Box::new(Orientation::Horizontal, 10);
        header.add_css_class("term-header");
        let badge = label("⚡", "launcher-head-badge");
        badge.set_valign(Align::Center);
        header.append(&badge);
        let titles = gtk4::Box::new(Orientation::Vertical, 0);
        titles.set_hexpand(true);
        titles.set_valign(Align::Center);
        let title = label("Welcome to SUPER DESKTOP", "term-title");
        title.set_halign(Align::Start);
        titles.append(&title);
        let step_label = label("", "launcher-subtitle");
        step_label.set_halign(Align::Start);
        titles.append(&step_label);
        header.append(&titles);
        let skip = gtk4::Button::with_label("Skip tour");
        skip.add_css_class("launcher-btn");
        skip.set_valign(Align::Center);
        header.append(&skip);
        let close = gtk4::Button::with_label("✕");
        close.add_css_class("term-btn");
        close.set_tooltip_text(Some("Close the tour [Esc]"));
        close.set_valign(Align::Center);
        header.append(&close);
        panel.append(&header);

        let stack = gtk4::Stack::new();
        stack.set_transition_type(if animations_enabled() {
            gtk4::StackTransitionType::SlideLeftRight
        } else {
            gtk4::StackTransitionType::None
        });
        stack.set_transition_duration(280);
        stack.set_vhomogeneous(true);
        panel.append(&stack);

        let (hide_scene, paint_hide) = scene_hide(&combo);
        let (done_scene, paint_done) = scene_done(&combo);
        let scenes = vec![
            scene_welcome(),
            scene_launch(),
            scene_add(),
            hide_scene,
            scene_phone(),
            scene_remote(),
            done_scene,
        ];
        let demo = gtk4::Button::with_label("▶  Try the interactive demo on the website");
        demo.add_css_class("launcher-btn");
        demo.add_css_class("tour-demo");
        demo.set_halign(Align::Center);
        let mut bodies = Vec::new();
        for (index, (scene, words)) in scenes.iter().zip(WORDS.iter()).enumerate() {
            let page = gtk4::Box::new(Orientation::Vertical, 6);
            page.add_css_class("tour-page");
            page.append(&scene.widget);
            let eyebrow = label(words.eyebrow, "tour-eyebrow");
            eyebrow.set_margin_top(12);
            page.append(&eyebrow);
            page.append(&label(words.title, "tour-title"));
            let body = label(&fill_combo(words.body, &combo.borrow()), "tour-body");
            body.set_wrap(true);
            body.set_justify(gtk4::Justification::Center);
            body.set_max_width_chars(78);
            body.set_width_chars(60);
            page.append(&body);
            if index == STEPS - 1 {
                page.append(&demo);
            }
            bodies.push(body);
            stack.add_named(&page, Some(&format!("step-{index}")));
        }

        let footer = gtk4::Box::new(Orientation::Horizontal, 10);
        footer.add_css_class("tour-footer");
        let back = gtk4::Button::with_label("Back");
        back.add_css_class("launcher-btn");
        footer.append(&back);
        let dot_row = gtk4::Box::new(Orientation::Horizontal, 6);
        dot_row.set_hexpand(true);
        dot_row.set_halign(Align::Center);
        dot_row.set_valign(Align::Center);
        let dots: Vec<gtk4::Button> = (0..STEPS)
            .map(|index| {
                let dot = gtk4::Button::new();
                dot.add_css_class("tour-dot");
                dot.set_valign(Align::Center);
                dot.set_tooltip_text(Some(WORDS[index].title));
                dot_row.append(&dot);
                dot
            })
            .collect();
        footer.append(&dot_row);
        let next = gtk4::Button::with_label("Next");
        next.add_css_class("launcher-btn");
        next.add_css_class("tour-next");
        footer.append(&next);
        panel.append(&footer);

        let tour = Rc::new(Self {
            widget,
            panel,
            stack,
            scenes,
            bodies,
            paint_keys: vec![paint_hide, paint_done],
            combo,
            dots,
            step_label,
            back: back.clone(),
            next: next.clone(),
            step: Cell::new(0),
            started: Cell::new(None),
            tick: RefCell::new(None),
            hooks,
        });

        let weak = Rc::downgrade(&tour);
        back.connect_clicked(move |_| {
            if let Some(tour) = weak.upgrade() {
                tour.go_to(tour.step.get().saturating_sub(1));
            }
        });
        let weak = Rc::downgrade(&tour);
        next.connect_clicked(move |_| {
            if let Some(tour) = weak.upgrade() {
                tour.advance();
            }
        });
        for (index, dot) in tour.dots.iter().enumerate() {
            let weak = Rc::downgrade(&tour);
            dot.connect_clicked(move |_| {
                if let Some(tour) = weak.upgrade() {
                    tour.go_to(index);
                }
            });
        }
        for button in [&skip, &close] {
            let weak = Rc::downgrade(&tour);
            button.connect_clicked(move |_| {
                if let Some(tour) = weak.upgrade() {
                    tour.close();
                }
            });
        }
        let weak = Rc::downgrade(&tour);
        demo.connect_clicked(move |_| {
            let Some(tour) = weak.upgrade() else { return };
            tour.close();
            glib::MainContext::default().spawn_local(async {
                if let Err(error) = gtk4::gio::AppInfo::launch_default_for_uri_future(
                    DEMO_URL,
                    None::<&gtk4::gio::AppLaunchContext>,
                )
                .await
                {
                    eprintln!("SUPER DESKTOP: cannot open {DEMO_URL}: {error}");
                }
            });
            // The browser opens under the overlay.
            (tour.hooks.hide_overlay)();
        });

        // ← → move between slides; Enter is the focused Next button.
        let keys = gtk4::EventControllerKey::new();
        keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let weak = Rc::downgrade(&tour);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(tour) = weak.upgrade() else { return glib::Propagation::Proceed };
            match key {
                gdk::Key::Right => tour.advance(),
                gdk::Key::Left => tour.go_to(tour.step.get().saturating_sub(1)),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        tour.widget.add_controller(keys);

        // Ticks run only while the tour is on screen.
        let weak = Rc::downgrade(&tour);
        tour.widget.connect_map(move |_| {
            if let Some(tour) = weak.upgrade() {
                tour.start_ticking();
            }
        });
        let weak = Rc::downgrade(&tour);
        tour.widget.connect_unmap(move |_| {
            if let Some(tour) = weak.upgrade() {
                tour.stop_ticking();
            }
        });
        tour.paint_step();
        tour
    }

    pub fn is_open(&self) -> bool {
        self.widget.is_visible()
    }

    /// Show the tour from its first slide, naming `combo` as the shortcut.
    pub fn open(&self, combo: &str) {
        self.set_combo(combo);
        self.widget.set_margin_top((self.hooks.top)());
        self.stack.set_visible_child_name("step-0");
        self.step.set(0);
        self.restart_slide();
        self.paint_step();
        self.widget.set_visible(true);
        self.next.grab_focus();
    }

    pub fn close(&self) {
        self.stop_ticking();
        self.widget.set_visible(false);
    }

    fn set_combo(&self, combo: &str) {
        if *self.combo.borrow() == combo {
            return;
        }
        *self.combo.borrow_mut() = combo.to_string();
        for (body, words) in self.bodies.iter().zip(WORDS.iter()) {
            body.set_text(&fill_combo(words.body, combo));
        }
        for paint in &self.paint_keys {
            paint(combo);
        }
    }

    fn advance(&self) {
        if self.step.get() + 1 >= STEPS {
            self.close();
        } else {
            self.go_to(self.step.get() + 1);
        }
    }

    fn go_to(&self, step: usize) {
        let step = step.min(STEPS - 1);
        if step == self.step.get() {
            return;
        }
        self.step.set(step);
        self.stack.set_visible_child_name(&format!("step-{step}"));
        self.restart_slide();
        self.paint_step();
    }

    fn paint_step(&self) {
        let step = self.step.get();
        self.step_label.set_text(&format!("A quick tour · {} of {STEPS}", step + 1));
        for (index, dot) in self.dots.iter().enumerate() {
            set_class(dot, "active", index == step);
        }
        self.back.set_sensitive(step > 0);
        self.next.set_label(if step + 1 == STEPS { "Start using SUPER DESKTOP" } else { "Next" });
    }

    /// Start the current slide's animation over (or show its still).
    fn restart_slide(&self) {
        self.started.set(None);
        let scene = &self.scenes[self.step.get()];
        (scene.animate)(if animations_enabled() { 0.0 } else { scene.still });
    }

    /// Pose the current slide at `t` seconds into its animation.
    fn render(&self, t: f64) {
        let scene = &self.scenes[self.step.get()];
        (scene.animate)(t.rem_euclid(scene.period));
    }

    fn start_ticking(self: &Rc<Self>) {
        if self.tick.borrow().is_some() || !animations_enabled() {
            return;
        }
        let weak = Rc::downgrade(self);
        let id = self.panel.add_tick_callback(move |_, clock| {
            let Some(tour) = weak.upgrade() else { return glib::ControlFlow::Break };
            let now = clock.frame_time() as f64 / 1_000_000.0;
            let start = tour.started.get().unwrap_or_else(|| {
                tour.started.set(Some(now));
                now
            });
            tour.render(now - start);
            glib::ControlFlow::Continue
        });
        *self.tick.borrow_mut() = Some(id);
    }

    fn stop_ticking(&self) {
        if let Some(id) = self.tick.borrow_mut().take() {
            id.remove();
        }
    }
}

/// The tour's stylesheet, in the Omarchy palette.
pub fn css(theme: &OmarchyTheme) -> String {
    format!(
        r#"
/* ================= Welcome tour ================= */
.tour-scrim {{
    background-color: rgba(0, 0, 0, 0.42);
}}
.tour-panel {{
    border: 1px solid {accent_soft};
    box-shadow: 0 24px 64px rgba(0, 0, 0, 0.6), 0 0 0 1px {accent_faint};
}}
.tour-page {{
    padding: 18px 28px 4px 28px;
}}
.tour-stage {{
    border-radius: 12px;
    border: 1px solid {line};
    background-color: {darker};
    background-image:
        radial-gradient(circle at 22% 18%, {accent_glow}, transparent 55%),
        radial-gradient(circle at 82% 8%, {magenta_glow}, transparent 50%);
}}
.tour-eyebrow {{
    color: {accent};
    font-size: 11px;
    font-weight: 800;
    letter-spacing: 2px;
}}
.tour-title {{
    color: {bright_fg};
    font-size: 24px;
    font-weight: 800;
}}
.tour-body {{
    color: {fg};
    font-size: 13.5px;
}}
.tour-footer {{
    padding: 12px 18px 16px 18px;
}}
button.tour-dot {{
    min-width: 8px;
    min-height: 8px;
    padding: 0;
    border: none;
    border-radius: 9999px;
    background-color: {line_strong};
    background-image: none;
    box-shadow: none;
    transition: min-width 200ms ease, background-color 200ms ease;
}}
button.tour-dot.active {{
    min-width: 24px;
    background-color: {accent};
}}
button.tour-next {{
    background-image: linear-gradient(90deg, {accent}, {magenta});
    color: {background};
    border: none;
    font-weight: 800;
    padding: 7px 18px;
}}
button.tour-demo {{
    margin-top: 10px;
    color: {demo_text};
    border-color: {accent_soft};
}}

/* The miniature overlay the slides animate. */
.tour-bar {{
    background-color: {bar_bg};
    border-bottom: 1px solid {accent_soft};
    padding: 0 10px;
}}
.tour-pc {{
    color: {fg};
    background-color: {chip_bg};
    border-radius: 6px;
    padding: 2px 7px;
    font-size: 10px;
    font-weight: 700;
}}
.tour-pc.remote {{
    color: {bright_blue};
    box-shadow: inset 0 0 0 1px {bright_blue};
}}
.tour-brand {{
    color: {accent};
    font-size: 10px;
    font-weight: 800;
}}
.tour-folder {{
    color: {dim_fg};
    font-family: '{font}', monospace;
    font-size: 9.5px;
    border-bottom: 1px solid {line_strong};
    padding: 1px 4px;
}}
.tour-launch {{
    border-radius: 6px;
    padding: 3px 7px;
}}
.tour-launch.hot {{
    background-color: {badge_bg};
    box-shadow: inset 0 -2px {accent}, 0 0 12px {accent_glow};
}}
.tour-launch-name {{
    color: {fg};
    font-size: 10px;
    font-weight: 600;
}}
.tour-launch-emoji {{
    font-size: 10px;
}}
.tour-icon {{
    color: {fg};
    margin-left: 4px;
}}
.tour-icon.hot {{
    color: {accent};
}}
.tour-card {{
    background-color: {card_bg};
    border: 1px solid {line};
    border-radius: 9px;
    box-shadow: 0 6px 18px rgba(0, 0, 0, 0.45);
}}
.tour-card.remote {{
    border: 1.5px solid {bright_blue};
}}
.tour-card-head {{
    background-color: {head_bg};
    border-bottom: 1px solid {line};
    padding: 4px 7px;
}}
.tour-card-title {{
    color: {bright_fg};
    font-size: 10px;
    font-weight: 700;
}}
.tour-card-body {{
    color: {fg};
    font-family: '{font}', monospace;
    font-size: 9.5px;
    padding: 6px 8px;
}}
.tour-status {{
    border-radius: 9999px;
    padding: 1px 6px;
    font-size: 8.5px;
    font-weight: 700;
}}
.tour-status.idle {{ color: {dim_fg}; background-color: {chip_bg}; }}
.tour-status.working {{ color: {bright_yellow}; background-color: {yellow_bg}; }}
.tour-status.finished {{ color: {bright_green}; background-color: {green_bg}; }}
.tour-ripple {{
    border: 2px solid {accent};
    border-radius: 9999px;
    background-color: {accent_glow};
}}
.tour-key {{
    color: {bright_fg};
    background-color: {chip_bg};
    border: 1px solid {line_strong};
    border-bottom-width: 3px;
    border-radius: 7px;
    padding: 4px 9px;
    font-family: '{font}', monospace;
    font-size: 12px;
    font-weight: 800;
    transition: background-color 120ms ease, color 120ms ease;
}}
.tour-keys-large .tour-key {{
    font-size: 16px;
    padding: 6px 12px;
}}
.tour-key.pressed {{
    color: {background};
    background-color: {accent};
    border-color: {accent};
    border-bottom-width: 1px;
    margin-top: 2px;
}}
.tour-key-plus {{
    color: {dim_fg};
    font-weight: 700;
}}
.tour-caption {{
    color: {bright_fg};
    background-color: {bar_bg};
    border: 1px solid {accent_soft};
    border-radius: 9999px;
    padding: 4px 12px;
    font-size: 11px;
    font-weight: 700;
}}
.tour-dim {{
    background-color: {dim_bg};
}}
.tour-browser {{
    background-color: {head_bg};
    border-radius: 8px;
    padding: 12px 16px;
}}
.tour-browser-url {{
    color: {dim_fg};
    font-size: 10px;
    margin-bottom: 6px;
}}
.tour-line {{
    background-color: {line_strong};
    border-radius: 4px;
}}
.tour-panel-rows {{
    padding: 8px;
}}
.tour-row {{
    padding: 4px 6px;
    border-radius: 6px;
}}
.tour-row.hot {{
    background-color: {badge_bg};
    box-shadow: inset 0 0 0 1px {accent};
}}
.tour-add-row .tour-row-name {{
    color: {accent};
}}
.tour-row-name {{
    color: {fg};
    font-size: 10px;
    font-weight: 600;
}}
.tour-toggle {{
    color: {background};
    background-color: {accent};
    border-radius: 9999px;
    padding: 1px 6px;
    font-size: 8px;
    font-weight: 800;
}}
.tour-toggle.off {{
    color: {dim_fg};
    background-color: {chip_bg};
}}
.tour-field {{
    color: {fg};
    font-family: '{font}', monospace;
    font-size: 9.5px;
    border: 1px solid {accent_soft};
    border-radius: 6px;
    padding: 4px 6px;
    min-height: 12px;
}}
.tour-button {{
    color: {accent};
    background-color: {badge_bg};
    border: 1px solid {accent_soft};
    border-radius: 6px;
    padding: 3px 8px;
    font-size: 9.5px;
    font-weight: 700;
}}
.tour-button.hot {{
    color: {background};
    background-color: {accent};
}}
.tour-menu {{
    background-color: {bar_bg};
    border: 1px solid {accent_soft};
    border-radius: 8px;
    padding: 4px;
    box-shadow: 0 8px 20px rgba(0, 0, 0, 0.5);
}}
.tour-menu-item {{
    border-radius: 5px;
    padding: 4px 8px;
}}
.tour-menu-item.hot {{
    background-color: {badge_bg};
}}
.tour-menu-note {{
    color: {dim_fg};
    font-size: 8.5px;
}}
.tour-remote-frame {{
    border: 2px solid {accent};
    border-radius: 10px;
}}
.tour-monitor {{
    background-color: {darker};
    border: 2px solid {line_strong};
    border-radius: 10px;
}}
.tour-phone {{
    background-color: {darker};
    border: 2px solid {line_strong};
    border-radius: 20px;
    padding: 12px 8px 10px 8px;
    box-shadow: 0 10px 26px rgba(0, 0, 0, 0.5);
}}
.tour-phone-title {{
    color: {bright_fg};
    font-size: 10px;
    font-weight: 800;
    margin-bottom: 4px;
}}
.tour-phone-row {{
    background-color: {card_bg};
    border: 1px solid {line};
    border-radius: 8px;
    padding: 5px 6px;
}}
.tour-phone-row.hot {{
    border-color: {accent};
}}
.tour-phone-state {{
    color: {dim_fg};
    font-size: 8.5px;
}}
.tour-banner {{
    color: {bright_green};
    background-color: {card_bg};
    border: 1px solid {green_line};
    border-radius: 8px;
    padding: 4px 8px;
    font-size: 10px;
    font-weight: 800;
    box-shadow: 0 6px 16px rgba(0, 0, 0, 0.5);
}}
.tour-pill {{
    color: {background};
    background-color: {accent};
    border-radius: 9999px;
    padding: 2px 8px;
    font-size: 9.5px;
    font-weight: 800;
}}
"#,
        accent = theme.accent,
        demo_text = theme.readable_text(&theme.lighter_background, 0.85, &[&theme.accent]),
        magenta = theme.magenta,
        background = theme.background,
        darker = theme.darker_background,
        fg = theme.foreground,
        bright_fg = theme.bright_foreground,
        dim_fg = theme.dark_foreground,
        bright_blue = theme.bright_blue,
        bright_yellow = theme.bright_yellow,
        bright_green = theme.bright_green,
        font = theme.font_family,
        accent_soft = theme.rgba_accent(0.3),
        accent_faint = theme.rgba_accent(0.08),
        accent_glow = theme.rgba_accent(0.18),
        magenta_glow = OmarchyTheme::hex_to_rgba(&theme.magenta, 0.16),
        badge_bg = theme.rgba_accent(0.15),
        line = theme.rgba_muted(0.3),
        line_strong = theme.rgba_muted(0.5),
        bar_bg = theme.rgba_dark_bg(0.95),
        chip_bg = theme.rgba_lighter_bg(0.7),
        card_bg = theme.rgba_dark_bg(0.96),
        head_bg = theme.rgba_lighter_bg(0.85),
        dim_bg = theme.rgba_darker_bg(0.55),
        yellow_bg = OmarchyTheme::hex_to_rgba(&theme.yellow, 0.18),
        green_bg = OmarchyTheme::hex_to_rgba(&theme.green, 0.18),
        green_line = OmarchyTheme::hex_to_rgba(&theme.green, 0.5),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_helpers_stay_in_range() {
        assert_eq!(span(-1.0, 0.0, 2.0), 0.0);
        assert_eq!(span(1.0, 0.0, 2.0), 0.5);
        assert_eq!(span(9.0, 0.0, 2.0), 1.0);
        assert_eq!(span(1.0, 1.0, 0.0), 1.0);
        for p in [0.0, 0.25, 0.5, 0.75, 1.0] {
            for ease in [ease_in, ease_out, ease_in_out] {
                let v = ease(p);
                assert!((0.0..=1.0).contains(&v), "{v}");
            }
        }
        assert_eq!((ease_in_out(0.0), ease_in_out(1.0)), (0.0, 1.0));
        // A loop is invisible at both ends, so it starts over without a jump.
        assert_eq!(loop_fade(0.0, 7.0), 0.0);
        assert_eq!(loop_fade(7.0, 7.0), 0.0);
        assert_eq!(loop_fade(3.5, 7.0), 1.0);
        assert!(ripple(0.9, 1.0).is_none() && ripple(1.5, 1.0).is_none());
        let (scale, opacity) = ripple(1.1, 1.0).unwrap();
        assert!(scale > 0.4 && opacity > 0.0 && opacity < 0.9);
    }

    #[test]
    fn typing_never_splits_a_character() {
        let text = "> ✓ füße";
        assert_eq!(typed(text, 0.0), "");
        assert_eq!(typed(text, 1.0), text);
        assert_eq!(typed(text, 5.0), text);
        for step in 0..=20 {
            let shown = typed(text, step as f64 / 20.0);
            assert!(text.starts_with(shown));
        }
    }

    #[test]
    fn the_pointer_arrives_at_each_point_on_time() {
        let path = [(0.0, (0.0, 0.0)), (1.0, (100.0, 0.0)), (2.0, (100.0, 50.0))];
        assert_eq!(follow(&path, 0.0, 0.5), (0.0, 0.0));
        assert_eq!(follow(&path, 1.0, 0.5), (100.0, 0.0));
        assert_eq!(follow(&path, 1.2, 0.5), (100.0, 0.0));
        assert_eq!(follow(&path, 3.0, 0.5), (100.0, 50.0));
    }

    #[test]
    fn the_shortcut_is_spelled_as_key_caps() {
        assert_eq!(combo_keys("SUPER + SHIFT + Q"), ["SUPER", "SHIFT", "Q"]);
        assert_eq!(combo_keys("CTRL + ALT + space"), ["CTRL", "ALT", "space"]);
        assert!(combo_keys("").is_empty());
    }

    #[test]
    fn every_slide_has_words_and_names_the_real_shortcut() {
        for words in &WORDS {
            assert!(!words.eyebrow.is_empty() && !words.title.is_empty() && !words.body.is_empty());
            let body = fill_combo(words.body, "SUPER + ALT + Space");
            assert!(!body.contains("{combo}"));
        }
        // The slides the user asked for: one home for everything, start any
        // harness, add one, hide, phone, other PCs.
        let all: String = WORDS.iter().map(|w| format!("{} {} ", w.title, w.body)).collect();
        for needle in ["every terminal", "launcher", "Add a harness", "Hide", "Android", "This PC menu"] {
            assert!(all.contains(needle), "no slide mentions `{needle}`");
        }
        assert!(fill_combo(WORDS[3].body, "SUPER + ALT + Space").contains("SUPER + ALT + Space"));
    }

    #[test]
    fn the_tour_steps_through_its_slides_and_animates_inside_its_stage() {
        crate::gtk_test::run_in_child_process("welcome_tour::tests::tour_inner");
    }

    /// Drives the real tour on the private display. With
    /// `SD_TOUR_SHOTS=<dir>` it also saves one picture per slide.
    #[test]
    fn tour_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        gtk4::Settings::default().unwrap().set_gtk_application_prefer_dark_theme(true);
        crate::styles::apply_styles();
        let hidden = Rc::new(Cell::new(false));
        let tour = WelcomeTour::new("SUPER + SHIFT + Q", TourHooks {
            top: Rc::new(|| 46),
            hide_overlay: Rc::new({
                let hidden = Rc::clone(&hidden);
                move || hidden.set(true)
            }),
        });
        let overlay = gtk4::Overlay::new();
        overlay.set_child(Some(&gtk4::Box::new(Orientation::Vertical, 0)));
        overlay.add_overlay(&tour.widget);
        let window = gtk4::Window::new();
        window.set_default_size(1000, 740);
        window.set_child(Some(&overlay));
        window.present();
        assert!(!tour.is_open());
        tour.open("CTRL + ALT + space");
        crate::gtk_test::pump(300);
        assert!(tour.is_open());
        assert_eq!(tour.widget.margin_top(), 46, "the top bar stays uncovered");
        assert!(tour.tick.borrow().is_some(), "an open tour animates");
        assert!(tour.bodies[3].text().contains("CTRL + ALT + space"));
        assert!(!tour.back.is_sensitive());
        assert_eq!(tour.scenes.len(), STEPS);

        // Every scene, at every moment of its loop, stays inside its stage
        // and the panel inside the screen.
        for step in 0..STEPS {
            tour.go_to(step);
            let scene = &tour.scenes[step];
            let mut t = -0.5;
            while t < scene.period * 2.0 {
                tour.render(t);
                t += 0.1;
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
            while scene.widget.width() == 0 && std::time::Instant::now() < deadline {
                crate::gtk_test::pump(50);
            }
            assert_eq!(
                (scene.widget.width(), scene.widget.height()),
                (STAGE.0 as i32, STAGE.1 as i32),
                "slide {step} grew its stage"
            );
            assert!(tour.panel.width() <= 1000 && tour.panel.height() <= 740 - 46, "slide {step}: {}x{}", tour.panel.width(), tour.panel.height());
        }
        if let Some(out) = std::env::var_os("SD_TOUR_SHOTS").map(std::path::PathBuf::from) {
            std::fs::create_dir_all(&out).unwrap();
            tour.stop_ticking();
            tour.stack.set_transition_type(gtk4::StackTransitionType::None);
            for step in 0..STEPS {
                tour.go_to(step);
                crate::gtk_test::pump(400);
                for t in [tour.scenes[step].still, 1.6, 4.4] {
                    tour.render(t);
                    crate::gtk_test::save_png(&tour.panel, &out.join(format!("tour-{}-{t:.1}.png", step + 1)));
                }
            }
            tour.start_ticking();
            tour.stack.set_transition_type(gtk4::StackTransitionType::SlideLeftRight);
        }

        // Next, Back and the dots move between slides; the last Next closes.
        tour.go_to(0);
        tour.next.emit_clicked();
        assert_eq!(tour.step.get(), 1);
        assert!(tour.back.is_sensitive());
        tour.back.emit_clicked();
        assert_eq!(tour.step.get(), 0);
        tour.dots[STEPS - 1].emit_clicked();
        assert_eq!(tour.step.get(), STEPS - 1);
        assert_eq!(tour.next.label().as_deref(), Some("Start using SUPER DESKTOP"));
        tour.next.emit_clicked();
        assert!(!tour.is_open());
        crate::gtk_test::pump(100);
        assert!(tour.tick.borrow().is_none(), "a closed tour stops animating");

        // Reopening starts over; Skip closes it.
        tour.open("SUPER + SHIFT + Q");
        assert_eq!(tour.step.get(), 0);
        assert!(tour.bodies[3].text().contains("SUPER + SHIFT + Q"));
        tour.close();
        assert!(!tour.is_open());
        assert!(!hidden.get());

        // The replay hook reaches whatever the overlay registered.
        let opened = Rc::new(Cell::new(0));
        set_replay(Rc::new({
            let opened = Rc::clone(&opened);
            move || opened.set(opened.get() + 1)
        }));
        replay();
        assert_eq!(opened.get(), 1);
        window.close();
    }
}
