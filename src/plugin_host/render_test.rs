//! `super-desktop plugin test` for window renderers
//! (`skills/super-desktop-plugin/references/testing.md#renderers`).
//!
//! Runs the plugin's `renderer.wasm` in the same interpreter, with the same
//! budget and the same output checks as the desktop, without GTK:
//!
//! - `renderer`: a built-in conformance run every renderer plugin gets. Frames
//!   with 0, 1, 8 and 128 cards on three screen sizes, a drag and a drop.
//!   Every frame must succeed within the fuel budget and the layout must settle.
//! - Scenarios with a `"renderer"` object: cards in, frames until the layout
//!   settles, then expectations about where and how each card is drawn.
use super::manifest::Manifest;
use super::renderer::{self, CardIn, Frame, Mode, Output, Rect, Renderer, FUEL};
use super::store;
use super::testing::Outcome;
use serde_json::Value;
use std::path::Path;

/// Frames run until the layout settles; more than this and it never does.
const MAX_FRAMES: usize = 600;
const DT_MS: f64 = 16.0;
const MIN_W: f64 = 320.0;
const MIN_H: f64 = 200.0;

fn load(dir: &Path, manifest: &Manifest) -> Result<Renderer, String> {
    let spec = manifest.contributes.renderer.as_ref().ok_or("the plugin has no renderer")?;
    let bytes = std::fs::read(dir.join(&spec.wasm)).map_err(|e| format!("cannot read {}: {e} (build it first)", spec.wasm))?;
    Renderer::load_with_params(&bytes, &std::env::temp_dir().join("sd-plugin-test-renderer.log"), spec.params.len())
}

struct Run {
    frames: usize,
    settled: bool,
    max_fuel: u64,
    last: Output,
    drops: Vec<renderer::DropTarget>,
}

/// Present `frame` until the renderer stops animating (or `frames` times).
/// `first` is applied to the first frame only (a drop is a one-frame flag).
fn run(renderer: &mut Renderer, mut frame: Frame, frames: Option<usize>) -> Result<Run, String> {
    let mut run = Run { frames: 0, settled: false, max_fuel: 0, last: Output { cards: Vec::new(), animating: false, drop: None }, drops: Vec::new() };
    let limit = frames.unwrap_or(MAX_FRAMES).clamp(1, MAX_FRAMES);
    for i in 0..limit {
        frame.time_ms = i as f64 * DT_MS;
        frame.dt_ms = if i == 0 { 0.0 } else { DT_MS };
        let output = renderer.present(&frame).map_err(|why| format!("frame {}: {why}", i + 1))?;
        run.frames = i + 1;
        run.max_fuel = run.max_fuel.max(renderer.last_fuel);
        if let Some(drop) = output.drop {
            run.drops.push(drop);
        }
        let animating = output.animating;
        run.last = output;
        // The dropped flag is set on one frame only, as on the desktop.
        for card in &mut frame.cards {
            card.flags &= !renderer::FLAG_DROPPED;
        }
        if !animating && frames.is_none() {
            run.settled = true;
            break;
        }
    }
    if frames.is_some() {
        run.settled = !run.last.animating;
    }
    Ok(run)
}

/// `n` cards spread over the screen: a grid of open cards, every fifth one an
/// icon, the first one focused.
fn spread(n: usize, w: f64, h: f64, top: f64) -> Vec<CardIn> {
    let columns = (n as f64).sqrt().ceil().max(1.0) as usize;
    (0..n)
        .map(|i| {
            let (col, row) = (i % columns, i / columns);
            let cell_w = w / columns as f64;
            let cell_h = (h - top) / columns as f64;
            let saved = Rect { x: col as f64 * cell_w, y: top + row as f64 * cell_h, w: 640.0, h: 420.0 };
            let mut flags = 0;
            if i % 5 == 4 {
                flags |= renderer::FLAG_ICONIFIED;
            }
            if i == 0 {
                flags |= renderer::FLAG_FOCUSED;
            }
            CardIn {
                id: i as u32 + 1,
                flags,
                saved,
                icon_x: saved.x,
                icon_y: saved.y,
                icon_side: 72.0,
                status: (i % 6) as u32,
                agent_hash: renderer::agent_hash(["claude", "codex", "shell"][i % 3]),
                z: i as u32,
                min_w: MIN_W,
                min_h: MIN_H,
            }
        })
        .collect()
}

fn fuel_words(fuel: u64) -> String {
    format!("{fuel} fuel ({}% of the budget)", fuel * 100 / FUEL)
}

/// The built-in conformance run.
pub fn conformance(dir: &Path, manifest: &Manifest) -> Outcome {
    let mut lines = Vec::new();
    let result = conformance_inner(dir, manifest, &mut lines);
    let ok = result.is_ok();
    if let Err(why) = result {
        lines.push(format!("FAIL {why}"));
    }
    Outcome { name: "renderer".into(), ok, lines }
}

fn conformance_inner(dir: &Path, manifest: &Manifest, lines: &mut Vec<String>) -> Result<(), String> {
    let params = renderer::params_of(manifest);
    let mut renderer = load(dir, manifest)?;
    lines.push(format!("ok   loads ({} params)", params.len()));
    for (w, h) in [(1920.0, 1080.0), (1024.0, 768.0), (3840.0, 2160.0)] {
        for n in [0, 1, 8, 128] {
            // Every size starts clean, as after activation.
            let mut renderer = load(dir, manifest)?;
            let frame = Frame { screen_w: w, screen_h: h, top: 46.0, cards: spread(n, w, h, 46.0), params: params.clone(), ..Frame::default() };
            let run = run(&mut renderer, frame, None).map_err(|why| format!("{n} cards on {w}×{h}: {why}"))?;
            if !run.settled {
                return Err(format!(
                    "{n} cards on {w}×{h}: still animating after {MAX_FRAMES} frames; set the animating flag only while something moves, or the desktop draws frames forever"
                ));
            }
            if run.last.cards.len() != n {
                return Err(format!("{n} cards on {w}×{h}: {} cards in the output", run.last.cards.len()));
            }
            if n == 128 || (n == 8 && w == 1920.0) {
                lines.push(format!("ok   {n} cards on {w}×{h}: settles in {} frames, at most {}", run.frames, fuel_words(run.max_fuel)));
            }
        }
    }
    // A drag from the left edge to the right edge, then a drop.
    let mut cards = spread(4, 1920.0, 1080.0, 46.0);
    cards[1].flags |= renderer::FLAG_DRAGGING;
    let mut max_fuel = 0;
    for step in 0..=60 {
        let x = -200.0 + step as f64 * 2200.0 / 60.0;
        cards[1].saved.x = x;
        let frame = Frame {
            screen_w: 1920.0,
            screen_h: 1080.0,
            top: 46.0,
            phase: 1,
            pointer: (x + cards[1].saved.w / 2.0, cards[1].saved.y + cards[1].saved.h / 2.0),
            pointer_down: true,
            cards: cards.clone(),
            params: params.clone(),
            ..Frame::default()
        };
        renderer.present(&frame).map_err(|why| format!("dragging across the screen, step {step}: {why}"))?;
        max_fuel = max_fuel.max(renderer.last_fuel);
    }
    cards[1].flags = (cards[1].flags & !renderer::FLAG_DRAGGING) | renderer::FLAG_DROPPED;
    let frame = Frame { screen_w: 1920.0, screen_h: 1080.0, top: 46.0, cards, params: params.clone(), ..Frame::default() };
    let run = run(&mut renderer, frame, None).map_err(|why| format!("after a drop: {why}"))?;
    if !run.settled {
        return Err(format!("still animating {MAX_FRAMES} frames after a drop"));
    }
    lines.push(format!("ok   drag across the screen and drop: at most {}", fuel_words(max_fuel.max(run.max_fuel))));
    Ok(())
}

/// A scenario file with a `"renderer"` object.
pub fn scenario(dir: &Path, manifest: &Manifest, spec: &Value, lines: &mut Vec<String>) -> Result<(), String> {
    let r = &spec["renderer"];
    if manifest.contributes.renderer.is_none() {
        return Err("the scenario has \"renderer\" but the plugin has no contributes.renderer".into());
    }
    // Settings: the scenario's on top of the defaults, checked like the Settings page does.
    if let Some(home) = std::env::var_os("SUPER_DESKTOP_PLUGIN_HOME") {
        let _ = std::fs::remove_dir_all(&home);
    }
    for (key, value) in spec["settings"].as_object().into_iter().flatten() {
        store::set_setting(manifest, key, value.clone()).map_err(|why| format!("settings.{key}: {why}"))?;
    }
    let screen = &r["screen"];
    let num = |v: &Value, key: &str, default: f64| v.get(key).and_then(Value::as_f64).unwrap_or(default);
    let (w, h, top) = (num(screen, "width", 1920.0), num(screen, "height", 1080.0), num(screen, "top", 46.0));
    let mut cards = Vec::new();
    for (i, c) in r["cards"].as_array().ok_or("renderer.cards: a list of cards is needed")?.iter().enumerate() {
        let id = c.get("id").and_then(Value::as_u64).filter(|id| *id > 0 && *id <= u64::from(u32::MAX)).ok_or(format!("renderer.cards[{i}].id: a positive number"))? as u32;
        let saved = Rect { x: num(c, "x", 0.0), y: num(c, "y", top), w: num(c, "width", 640.0), h: num(c, "height", 420.0) };
        let mut flags = 0;
        for (key, flag) in [
            ("iconified", renderer::FLAG_ICONIFIED),
            ("expanded", renderer::FLAG_EXPANDED),
            ("dragging", renderer::FLAG_DRAGGING),
            ("focused", renderer::FLAG_FOCUSED),
            ("dropped", renderer::FLAG_DROPPED),
        ] {
            if c.get(key).and_then(Value::as_bool) == Some(true) {
                flags |= flag;
            }
        }
        cards.push(CardIn {
            id,
            flags,
            saved,
            icon_x: num(c, "iconX", saved.x),
            icon_y: num(c, "iconY", saved.y),
            icon_side: 72.0,
            status: 0,
            agent_hash: renderer::agent_hash(c.get("agent").and_then(Value::as_str).unwrap_or("shell")),
            z: i as u32,
            min_w: MIN_W,
            min_h: MIN_H,
        });
    }
    let focused = cards.iter().find(|c| c.flags & renderer::FLAG_FOCUSED != 0).map_or(0, |c| c.id);
    let frame = Frame { screen_w: w, screen_h: h, top, focused, cards, params: renderer::params_of(manifest), ..Frame::default() };
    let mut renderer = load(dir, manifest)?;
    let frames = r.get("frames").and_then(Value::as_u64).map(|n| n as usize);
    let run = run(&mut renderer, frame, frames)?;
    lines.push(format!("ok   {} frames, {}, at most {}", run.frames, if run.settled { "settled" } else { "still animating" }, fuel_words(run.max_fuel)));
    for (i, expect) in r["expect"].as_array().into_iter().flatten().enumerate() {
        check(expect, &run).map_err(|why| format!("renderer.expect[{i}] {expect}: {why}"))?;
        lines.push(format!("ok   expect {expect}"));
    }
    Ok(())
}

/// `5` is 5 ± 1; `{"min": a, "max": b}` is a range (either end optional).
fn matches(value: f64, wanted: &Value) -> bool {
    match wanted {
        Value::Number(n) => n.as_f64().is_some_and(|n| (value - n).abs() <= 1.0),
        Value::Object(range) => {
            range.get("min").and_then(Value::as_f64).is_none_or(|min| value >= min) && range.get("max").and_then(Value::as_f64).is_none_or(|max| value <= max)
        }
        _ => false,
    }
}

fn check(expect: &Value, run: &Run) -> Result<(), String> {
    if let Some(settled) = expect.get("settled").and_then(Value::as_bool) {
        if settled != run.settled {
            return Err(if settled { "the layout is still animating".into() } else { "the layout settled".into() });
        }
    }
    if let Some(drop) = expect.get("dropTarget") {
        let card = drop.get("card").and_then(Value::as_u64).unwrap_or(0) as u32;
        let found = run.drops.iter().find(|d| d.id == card).ok_or(format!("no drop target for card {card} (drops: {:?})", run.drops))?;
        if let Some(to_icon) = drop.get("toIcon").and_then(Value::as_bool) {
            if found.to_icon != to_icon {
                return Err(format!("drop target toIcon is {}", found.to_icon));
            }
        }
        for (key, value) in [("x", found.x), ("y", found.y)] {
            if let Some(wanted) = drop.get(key) {
                if !matches(value, wanted) {
                    return Err(format!("drop target {key} is {value:.1}"));
                }
            }
        }
    }
    if expect.get("noDropTarget").and_then(Value::as_bool) == Some(true) && !run.drops.is_empty() {
        return Err(format!("unexpected drop target {:?}", run.drops));
    }
    if let Some(card) = expect.get("card").and_then(Value::as_u64) {
        let out = run.last.cards.iter().find(|c| u64::from(c.id) == card).ok_or(format!("card {card} is not in the output"))?;
        let r = out.rect;
        if let Some(mode) = expect.get("mode").and_then(Value::as_str) {
            let actual = match out.mode {
                Mode::Icon => "icon",
                Mode::Full => "full",
                Mode::Resized => "resized",
            };
            if mode != actual {
                return Err(format!("card {card} is drawn as {actual} at {:.0},{:.0} {:.0}×{:.0}", r.x, r.y, r.w, r.h));
            }
        }
        for (key, value) in [
            ("x", r.x),
            ("y", r.y),
            ("width", r.w),
            ("height", r.h),
            ("centerX", r.x + r.w / 2.0),
            ("centerY", r.y + r.h / 2.0),
            ("opacity", out.opacity),
        ] {
            if let Some(wanted) = expect.get(key) {
                if !matches(value, wanted) {
                    return Err(format!("card {card} {key} is {value:.1} (drawn at {:.0},{:.0} {:.0}×{:.0})", r.x, r.y, r.w, r.h));
                }
            }
        }
    }
    Ok(())
}

/// Whether a scenario file is a renderer scenario.
pub fn is_renderer_scenario(spec: &Value) -> bool {
    spec.get("renderer").is_some_and(Value::is_object)
}
