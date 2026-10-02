//! Center Magnify: a SUPER DESKTOP window renderer (renderer ABI 1).
//!
//! The closer a card's centre is to the horizontal centre of the screen, the
//! larger it is drawn: up to 70% of the screen width (the `maxWidth`
//! setting). Moving it toward a side shrinks it smoothly down to `minWidth`,
//! and in the edge bands (`edgeBand`) it becomes an icon docked at the left
//! or right edge. Dropping a card in an edge band saves it as an icon there,
//! so turning the plugin off keeps it an icon; dragging an icon toward the
//! centre grows it back into a card, and dropping it there opens it.
//!
//! Settings arrive as `params` after the cards, in the manifest's
//! `renderer.params` order: maxWidth %, minWidth %, edgeBand %, iconSize px,
//! smoothing ms. Missing or out-of-range values fall back to the defaults.
//!
//! Build: `cargo build --release --target wasm32-unknown-unknown`, then copy
//! `target/wasm32-unknown-unknown/release/center_magnify.wasm` to `renderer.wasm`.
//! The byte layout is documented in `references/renderer-abi.md`.
#![cfg_attr(target_arch = "wasm32", no_std)]

pub const ABI: u32 = 1;
pub const IN_MAGIC: u32 = u32::from_le_bytes(*b"SDRI");
pub const OUT_MAGIC: u32 = u32::from_le_bytes(*b"SDRO");
pub const MAX_CARDS: usize = 128;
pub const STATE_CAP: usize = 4096;
pub const IN_HEADER: usize = 64;
pub const IN_CARD: usize = 64;
pub const IN_CARDS_AT: usize = IN_HEADER + STATE_CAP;
pub const MAX_PARAMS: usize = 16;
/// Room for 128 cards and the 16 params that may follow them.
pub const IN_CAP: usize = IN_CARDS_AT + MAX_CARDS * IN_CARD + 4 * MAX_PARAMS;
pub const OUT_HEADER: usize = 48;
pub const OUT_CARD: usize = 32;
pub const OUT_CARDS_AT: usize = OUT_HEADER + STATE_CAP;
pub const OUT_CAP: usize = OUT_CARDS_AT + MAX_CARDS * OUT_CARD;

pub const FLAG_ICONIFIED: u32 = 1 << 0;
pub const FLAG_EXPANDED: u32 = 1 << 1;
pub const FLAG_DRAGGING: u32 = 1 << 2;
pub const FLAG_FOCUSED: u32 = 1 << 3;
pub const FLAG_DROPPED: u32 = 1 << 5;
pub const MODE_FULL: u32 = 0;
pub const MODE_ICON: u32 = 1;

const GAP: f32 = 8.0;
const STATE_ENTRY: usize = 20; // id + x, y, w, h
/// The first frame (dt 0, as after turning on) still animates: one frame step.
const FIRST_DT_MS: f32 = 16.0;

/// The settings, from the frame's params (renderer-abi.md, "Params").
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tuning {
    /// Width fraction of a card at the centre.
    pub max: f32,
    /// Width fraction of a card just inside the edge band.
    pub min: f32,
    /// |distance from centre| / half width beyond which a card is an icon.
    pub edge: f32,
    /// Icon side in px.
    pub icon: f32,
    /// Smoothing time constant in ms; 0 jumps.
    pub tau: f32,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning { max: 0.70, min: 0.22, edge: 0.88, icon: 72.0, tau: 90.0 }
    }
}

impl Tuning {
    /// `params[i]` when present and within `lo..=hi`, else the default.
    pub fn from_params(params: &[f32]) -> Tuning {
        let d = Tuning::default();
        let get = |i: usize, lo: f32, hi: f32, default: f32| match params.get(i) {
            Some(v) if v.is_finite() && *v >= lo && *v <= hi => *v,
            _ => default,
        };
        let max = get(0, 20.0, 95.0, d.max * 100.0) / 100.0;
        let min = (get(1, 5.0, 60.0, d.min * 100.0) / 100.0).min(max);
        let edge = 1.0 - get(2, 2.0, 40.0, (1.0 - d.edge) * 100.0) / 100.0;
        Tuning { max, min, edge, icon: get(3, 48.0, 160.0, d.icon), tau: get(4, 0.0, 1000.0, d.tau) }
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Copy, Default)]
struct Card {
    id: u32,
    flags: u32,
    saved: Rect,
    icon_x: f32,
    icon_y: f32,
    z: u32,
    min_w: f32,
    min_h: f32,
}

fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn rd_f32(b: &[u8], at: usize) -> f32 {
    let v = f32::from_bits(rd_u32(b, at));
    if v.is_finite() { v } else { 0.0 }
}
fn wr_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn wr_f32(b: &mut [u8], at: usize, v: f32) {
    wr_u32(b, at, v.to_bits());
}
fn absf(v: f32) -> f32 {
    if v < 0.0 { -v } else { v }
}
fn clampf(v: f32, lo: f32, hi: f32) -> f32 {
    if hi < lo { lo } else if v < lo { lo } else if v > hi { hi } else { v }
}
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clampf((x - e0) / (e1 - e0), 0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Errors returned to the host as negative lengths.
pub const ERR_LENGTH: i32 = -1;
pub const ERR_HEADER: i32 = -2;
pub const ERR_COUNT: i32 = -3;

/// Reads one input frame and writes one output frame. Returns the output length.
pub fn present(input: &[u8], out: &mut [u8]) -> Result<usize, i32> {
    if input.len() < IN_CARDS_AT || out.len() < OUT_CAP {
        return Err(ERR_LENGTH);
    }
    if rd_u32(input, 0) != IN_MAGIC || rd_u32(input, 4) != ABI {
        return Err(ERR_HEADER);
    }
    let (sw, sh, top) = (rd_f32(input, 8), rd_f32(input, 12), rd_f32(input, 16));
    let dt = clampf(rd_f32(input, 32), 0.0, 100.0);
    let dt = if dt <= 0.0 { FIRST_DT_MS } else { dt };
    let n = rd_u32(input, 52) as usize;
    if n > MAX_CARDS {
        return Err(ERR_COUNT);
    }
    if input.len() < IN_CARDS_AT + n * IN_CARD {
        return Err(ERR_LENGTH);
    }
    let state_len = (rd_u32(input, 56) as usize).min(STATE_CAP);
    let state = &input[IN_HEADER..IN_HEADER + state_len];
    // Params follow the cards (a host without them sends none).
    let mut params = [0f32; MAX_PARAMS];
    let params_at = IN_CARDS_AT + n * IN_CARD;
    let count = (rd_u32(input, 60) as usize).min(MAX_PARAMS).min(input.len().saturating_sub(params_at) / 4);
    for (i, p) in params.iter_mut().enumerate().take(count) {
        *p = f32::from_bits(rd_u32(input, params_at + 4 * i));
    }
    let t = Tuning::from_params(&params[..count]);
    let (edge, icon) = (t.edge, t.icon);

    let mut cards = [Card::default(); MAX_CARDS];
    for (i, card) in cards.iter_mut().enumerate().take(n) {
        let at = IN_CARDS_AT + i * IN_CARD;
        *card = Card {
            id: rd_u32(input, at),
            flags: rd_u32(input, at + 4),
            saved: Rect { x: rd_f32(input, at + 8), y: rd_f32(input, at + 12), w: rd_f32(input, at + 16), h: rd_f32(input, at + 20) },
            icon_x: rd_f32(input, at + 24),
            icon_y: rd_f32(input, at + 28),
            z: rd_u32(input, at + 44),
            min_w: rd_f32(input, at + 48),
            min_h: rd_f32(input, at + 52),
        };
    }

    // Targets.
    let half = (sw / 2.0).max(1.0);
    let avail = (sh - top).max(1.0);
    let mut target = [Rect::default(); MAX_CARDS];
    let mut mode = [MODE_FULL; MAX_CARDS];
    let mut dock_left = [0usize; MAX_CARDS];
    let mut dock_right = [0usize; MAX_CARDS];
    let (mut nl, mut nr) = (0usize, 0usize);
    // (card, x, y, to_icon)
    let mut drop: Option<(u32, f32, f32, bool)> = None;

    for i in 0..n {
        let c = cards[i];
        let iconified = c.flags & FLAG_ICONIFIED != 0;
        let (cx, cy) = if iconified {
            (c.icon_x + icon / 2.0, c.icon_y + icon / 2.0)
        } else {
            (c.saved.x + c.saved.w / 2.0, c.saved.y + c.saved.h / 2.0)
        };
        let d = clampf(absf(cx - half) / half, 0.0, 1.0);
        let dragging = c.flags & FLAG_DRAGGING != 0;
        // An icon dropped outside the edge bands opens, centred where it fell.
        if c.flags & FLAG_DROPPED != 0 && iconified && d <= edge && drop.is_none() {
            drop = Some((c.id, cx - c.saved.w / 2.0, cy - c.saved.h / 2.0, false));
        }
        if (iconified || d > edge) && !dragging {
            mode[i] = MODE_ICON;
            if cx < half { dock_left[nl] = i; nl += 1 } else { dock_right[nr] = i; nr += 1 }
            if c.flags & FLAG_DROPPED != 0 && !iconified && drop.is_none() {
                let x = if cx < half { GAP } else { sw - icon - GAP };
                drop = Some((c.id, x, clampf(cy - icon / 2.0, top + GAP, sh - icon - GAP), true));
            }
            continue;
        }
        if d > edge {
            // Being dragged through an edge band: preview as an icon under the pointer.
            mode[i] = MODE_ICON;
            target[i] = Rect { x: cx - icon / 2.0, y: cy - icon / 2.0, w: icon, h: icon };
            continue;
        }
        let scale = t.max + (t.min - t.max) * smoothstep(0.0, edge, d);
        let ratio = clampf(if c.saved.h > 1.0 { c.saved.w / c.saved.h } else { 1.4 }, 0.5, 2.5);
        let mut w = scale * sw;
        let mut h = w / ratio;
        if h > avail * 0.92 {
            h = avail * 0.92;
            w = h * ratio;
        }
        w = w.max(c.min_w);
        h = h.max(c.min_h);
        target[i] = Rect {
            x: clampf(cx - w / 2.0, 0.0, sw - w),
            y: clampf(cy - h / 2.0, top, sh - h),
            w,
            h,
        };
    }

    // Dock icons along each edge in the order of their saved height.
    for (list, count, left) in [(&mut dock_left, nl, true), (&mut dock_right, nr, false)] {
        let ys = |i: usize| {
            let c = cards[i];
            if c.flags & FLAG_ICONIFIED != 0 { c.icon_y } else { c.saved.y }
        };
        for a in 1..count {
            let mut b = a;
            while b > 0 && ys(list[b - 1]) > ys(list[b]) {
                list.swap(b - 1, b);
                b -= 1;
            }
        }
        for (k, &i) in list.iter().enumerate().take(count) {
            let x = if left { GAP } else { sw - icon - GAP };
            let y = clampf(top + GAP + k as f32 * (icon + GAP), top, sh - icon);
            target[i] = Rect { x, y, w: icon, h: icon };
        }
    }

    // Smooth from the previous frame toward the targets.
    let alpha = if t.tau <= 0.0 { 1.0 } else { dt / (t.tau + dt) };
    let prev_count = if state.len() >= 4 { (rd_u32(state, 0) as usize).min((STATE_CAP - 4) / STATE_ENTRY) } else { 0 };
    let entry = |k: usize, id: u32| -> Option<Rect> {
        let at = 4 + k * STATE_ENTRY;
        if k >= prev_count || at + STATE_ENTRY > state.len() || rd_u32(state, at) != id {
            return None;
        }
        Some(Rect { x: rd_f32(state, at + 4), y: rd_f32(state, at + 8), w: rd_f32(state, at + 12), h: rd_f32(state, at + 16) })
    };
    // State is written in card order, so the same slot almost always matches;
    // scan only when cards were added or removed. Keeps 128 cards well inside the budget.
    let previous = |slot: usize, id: u32| -> Option<Rect> {
        entry(slot, id).or_else(|| (0..prev_count).find_map(|k| entry(k, id)))
    };
    let mut animating = false;
    let mut drawn = [Rect::default(); MAX_CARDS];
    for i in 0..n {
        let c = cards[i];
        let goal = target[i];
        let from = previous(i, c.id).unwrap_or(if c.flags & FLAG_ICONIFIED != 0 {
            Rect { x: c.icon_x, y: c.icon_y, w: icon, h: icon }
        } else {
            c.saved
        });
        let follow = c.flags & FLAG_DRAGGING != 0; // the dragged card stays under the pointer
        let mix = |a: f32, b: f32, k: f32| a + (b - a) * k;
        let r = Rect {
            x: if follow { goal.x } else { mix(from.x, goal.x, alpha) },
            y: if follow { goal.y } else { mix(from.y, goal.y, alpha) },
            w: mix(from.w, goal.w, alpha),
            h: mix(from.h, goal.h, alpha),
        };
        if absf(r.x - goal.x) + absf(r.y - goal.y) + absf(r.w - goal.w) + absf(r.h - goal.h) > 0.5 {
            animating = true;
        }
        drawn[i] = r;
    }

    // Output.
    out[..OUT_CARDS_AT + n * OUT_CARD].fill(0);
    wr_u32(out, 0, OUT_MAGIC);
    wr_u32(out, 4, ABI);
    wr_u32(out, 8, n as u32);
    wr_u32(out, 12, (animating as u32) | ((drop.is_some() as u32) << 1));
    if let Some((id, x, y, to_icon)) = drop {
        wr_u32(out, 16, id);
        wr_f32(out, 20, x);
        wr_f32(out, 24, y);
        wr_u32(out, 28, to_icon as u32);
    }
    let state_len = 4 + n * STATE_ENTRY;
    wr_u32(out, 32, state_len as u32);
    let s = OUT_HEADER;
    wr_u32(out, s, n as u32);
    for i in 0..n {
        let at = s + 4 + i * STATE_ENTRY;
        let r = drawn[i];
        wr_u32(out, at, cards[i].id);
        wr_f32(out, at + 4, r.x);
        wr_f32(out, at + 8, r.y);
        wr_f32(out, at + 12, r.w);
        wr_f32(out, at + 16, r.h);
    }
    for i in 0..n {
        let c = cards[i];
        let at = OUT_CARDS_AT + i * OUT_CARD;
        let r = drawn[i];
        let lift = if c.flags & (FLAG_FOCUSED | FLAG_DRAGGING) != 0 { 1000 } else { 0 };
        wr_u32(out, at, c.id);
        wr_f32(out, at + 4, r.x);
        wr_f32(out, at + 8, r.y);
        wr_f32(out, at + 12, r.w);
        wr_f32(out, at + 16, r.h);
        wr_u32(out, at + 20, mode[i]);
        wr_f32(out, at + 24, if mode[i] == MODE_ICON { 0.95 } else { 1.0 });
        wr_u32(out, at + 28, c.z + lift);
    }
    Ok(OUT_CARDS_AT + n * OUT_CARD)
}

// ---- WASM exports -----------------------------------------------------------
static mut INPUT: [u8; IN_CAP] = [0; IN_CAP];
static mut OUTPUT: [u8; OUT_CAP] = [0; OUT_CAP];

#[no_mangle]
pub extern "C" fn sd_abi_version() -> i32 {
    ABI as i32
}

#[no_mangle]
pub extern "C" fn sd_input() -> i32 {
    core::ptr::addr_of_mut!(INPUT) as usize as i32
}

#[no_mangle]
pub extern "C" fn sd_output() -> i32 {
    core::ptr::addr_of_mut!(OUTPUT) as usize as i32
}

#[no_mangle]
pub extern "C" fn sd_present(len: i32) -> i32 {
    if len < 0 || len as usize > IN_CAP {
        return ERR_LENGTH;
    }
    // SAFETY: the host calls sd_present from one thread and never during another call.
    let (input, output) = unsafe { (&*core::ptr::addr_of!(INPUT), &mut *core::ptr::addr_of_mut!(OUTPUT)) };
    match present(&input[..len as usize], output) {
        Ok(n) => n as i32,
        Err(code) => code,
    }
}

#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f32 = 1920.0;
    const H: f32 = 1080.0;
    const TOP: f32 = 46.0;
    const ICON: f32 = 72.0; // the default icon size

    struct In {
        cards: Vec<(u32, u32, Rect)>,
        state: Vec<u8>,
        dt: f32,
        params: Vec<f32>,
    }

    fn frame(spec: &In) -> Vec<u8> {
        let mut b = vec![0u8; IN_CARDS_AT + spec.cards.len() * IN_CARD + 4 * spec.params.len()];
        wr_u32(&mut b, 0, IN_MAGIC);
        wr_u32(&mut b, 4, ABI);
        wr_f32(&mut b, 8, W);
        wr_f32(&mut b, 12, H);
        wr_f32(&mut b, 16, TOP);
        wr_f32(&mut b, 32, spec.dt);
        wr_u32(&mut b, 52, spec.cards.len() as u32);
        wr_u32(&mut b, 56, spec.state.len() as u32);
        wr_u32(&mut b, 60, spec.params.len() as u32);
        for (i, p) in spec.params.iter().enumerate() {
            wr_f32(&mut b, IN_CARDS_AT + spec.cards.len() * IN_CARD + 4 * i, *p);
        }
        b[IN_HEADER..IN_HEADER + spec.state.len()].copy_from_slice(&spec.state);
        for (i, (id, flags, r)) in spec.cards.iter().enumerate() {
            let at = IN_CARDS_AT + i * IN_CARD;
            wr_u32(&mut b, at, *id);
            wr_u32(&mut b, at + 4, *flags);
            wr_f32(&mut b, at + 8, r.x);
            wr_f32(&mut b, at + 12, r.y);
            wr_f32(&mut b, at + 16, r.w);
            wr_f32(&mut b, at + 20, r.h);
            wr_f32(&mut b, at + 24, r.x);
            wr_f32(&mut b, at + 28, r.y);
            wr_f32(&mut b, at + 48, 320.0);
            wr_f32(&mut b, at + 52, 200.0);
        }
        b
    }

    fn card_out(out: &[u8], i: usize) -> (u32, Rect, u32) {
        let at = OUT_CARDS_AT + i * OUT_CARD;
        let r = Rect { x: rd_f32(out, at + 4), y: rd_f32(out, at + 8), w: rd_f32(out, at + 12), h: rd_f32(out, at + 16) };
        (rd_u32(out, at), r, rd_u32(out, at + 20))
    }

    fn state_of(out: &[u8]) -> Vec<u8> {
        let len = rd_u32(out, 32) as usize;
        out[OUT_HEADER..OUT_HEADER + len].to_vec()
    }

    fn settle(cards: Vec<(u32, u32, Rect)>) -> Vec<u8> {
        settle_with(cards, vec![])
    }

    fn settle_with(cards: Vec<(u32, u32, Rect)>, params: Vec<f32>) -> Vec<u8> {
        let mut out = vec![0u8; OUT_CAP];
        let mut state = Vec::new();
        for _ in 0..200 {
            present(&frame(&In { cards: cards.clone(), state: state.clone(), dt: 16.0, params: params.clone() }), &mut out).unwrap();
            state = state_of(&out);
        }
        out
    }

    fn centred(w: f32, h: f32) -> Rect {
        Rect { x: W / 2.0 - w / 2.0, y: 400.0, w, h }
    }

    #[test]
    fn a_card_at_the_centre_is_seventy_percent_wide() {
        // Focused or not: size follows the distance from the centre only.
        for flags in [0, FLAG_FOCUSED] {
            let out = settle(vec![(7, flags, centred(640.0, 300.0))]);
            let (id, r, mode) = card_out(&out, 0);
            assert_eq!((id, mode), (7, MODE_FULL));
            assert!((r.w - 0.70 * W).abs() < 2.0, "{r:?}");
            assert!(r.x >= 0.0 && r.x + r.w <= W + 0.5 && r.y >= TOP);
        }
    }

    #[test]
    fn size_falls_steadily_toward_the_sides() {
        let mut last = f32::MAX;
        for x in [960.0, 1150.0, 1350.0, 1550.0, 1700.0] {
            let w = card_out(&settle(vec![(1, 0, Rect { x: x - 320.0, y: 400.0, w: 640.0, h: 300.0 })]), 0).1.w;
            assert!(w < last, "{x}: {w} after {last}");
            last = w;
        }
    }

    #[test]
    fn settings_change_the_sizes() {
        let card = vec![(1, 0, centred(640.0, 300.0))];
        let half = card_out(&settle_with(card.clone(), vec![50.0]), 0).1;
        assert!((half.w - 0.50 * W).abs() < 2.0, "maxWidth 50: {half:?}");
        // A wide edge band (40%) turns a card 77% of the way out into an icon.
        let out_there = vec![(1, 0, Rect { x: 1700.0 - 320.0, y: 400.0, w: 640.0, h: 300.0 })];
        assert_eq!(card_out(&settle_with(out_there.clone(), vec![]), 0).2, MODE_FULL);
        assert_eq!(card_out(&settle_with(out_there, vec![70.0, 22.0, 40.0]), 0).2, MODE_ICON);
        // Icon size, and out-of-range values fall back to the defaults.
        let icon = card_out(&settle_with(vec![(2, FLAG_ICONIFIED, Rect { x: 10.0, y: 300.0, w: 72.0, h: 72.0 })], vec![70.0, 22.0, 12.0, 96.0]), 0).1;
        assert!((icon.w - 96.0).abs() < 1.0, "{icon:?}");
        assert_eq!(Tuning::from_params(&[500.0, f32::NAN, -1.0, 9999.0, -5.0]), Tuning::default());
    }

    #[test]
    fn an_icon_dragged_to_the_centre_grows_into_a_card() {
        let dragged = vec![(3, FLAG_ICONIFIED | FLAG_DRAGGING, Rect { x: 900.0, y: 400.0, w: 640.0, h: 300.0 })];
        let (_, r, mode) = card_out(&settle(dragged), 0);
        assert_eq!(mode, MODE_FULL);
        assert!(r.w > 0.6 * W, "{r:?}");
    }

    #[test]
    fn card_shrinks_away_from_centre() {
        let near = card_out(&settle(vec![(1, 0, centred(640.0, 480.0))]), 0).1;
        let far = card_out(&settle(vec![(1, 0, Rect { x: 1350.0, y: 400.0, w: 640.0, h: 480.0 })]), 0).1;
        assert!(far.w < near.w, "near {near:?} far {far:?}");
    }

    #[test]
    fn edge_cards_dock_as_icons_without_overlapping() {
        let out = settle(vec![
            (1, 0, Rect { x: 10.0, y: 300.0, w: 100.0, h: 100.0 }),
            (2, 0, Rect { x: 0.0, y: 600.0, w: 100.0, h: 100.0 }),
            (3, FLAG_ICONIFIED, Rect { x: 1850.0, y: 500.0, w: 72.0, h: 72.0 }),
        ]);
        let (a, b, c) = (card_out(&out, 0), card_out(&out, 1), card_out(&out, 2));
        assert!(a.2 == MODE_ICON && b.2 == MODE_ICON && c.2 == MODE_ICON);
        assert!((a.1.x - GAP).abs() < 1.0 && (b.1.x - GAP).abs() < 1.0);
        assert!(b.1.y >= a.1.y + ICON, "{:?} {:?}", a.1, b.1);
        assert!((c.1.x - (W - ICON - GAP)).abs() < 1.0);
    }

    #[test]
    fn drop_in_edge_band_asks_to_save_an_icon() {
        let mut out = vec![0u8; OUT_CAP];
        let spec = In { cards: vec![(9, FLAG_DROPPED, Rect { x: 1800.0, y: 500.0, w: 200.0, h: 150.0 })], state: vec![], dt: 16.0, params: vec![] };
        present(&frame(&spec), &mut out).unwrap();
        assert_eq!(rd_u32(&out, 12) & 2, 2);
        assert_eq!(rd_u32(&out, 16), 9);
        assert_eq!(rd_u32(&out, 28), 1);
        assert!((rd_f32(&out, 20) - (W - ICON - GAP)).abs() < 1.0);
    }

    #[test]
    fn an_icon_dropped_in_the_middle_opens() {
        let mut out = vec![0u8; OUT_CAP];
        // Iconified, its icon spot dragged to the middle; saved open size 640×480.
        let mut spec = In { cards: vec![(4, FLAG_ICONIFIED | FLAG_DROPPED, Rect { x: 900.0, y: 400.0, w: 640.0, h: 480.0 })], state: vec![], dt: 16.0, params: vec![] };
        present(&frame(&spec), &mut out).unwrap();
        assert_eq!(rd_u32(&out, 12) & 2, 2);
        assert_eq!((rd_u32(&out, 16), rd_u32(&out, 28)), (4, 0), "open it, not keep it an icon");
        // Without the drop it stays an icon.
        spec.cards[0].1 = FLAG_ICONIFIED;
        present(&frame(&spec), &mut out).unwrap();
        assert_eq!(rd_u32(&out, 12) & 2, 0);
    }

    #[test]
    fn turning_on_animates_from_the_saved_layout() {
        let mut out = vec![0u8; OUT_CAP];
        let spec = In { cards: vec![(1, 0, centred(640.0, 300.0))], state: vec![], dt: 0.0, params: vec![] };
        present(&frame(&spec), &mut out).unwrap();
        assert_eq!(rd_u32(&out, 12) & 1, 1, "the first frame (dt 0) does not jump");
        assert!(card_out(&out, 0).1.w < 0.5 * W);
    }

    #[test]
    fn room_for_128_cards_and_16_params() {
        let mut out = vec![0u8; OUT_CAP];
        let cards = (1..=128).map(|i| (i, 0, Rect { x: (i * 13) as f32, y: 300.0, w: 640.0, h: 300.0 })).collect();
        let spec = In { cards, state: vec![], dt: 16.0, params: vec![70.0; MAX_PARAMS] };
        let input = frame(&spec);
        assert_eq!(input.len(), IN_CAP);
        assert!(present(&input, &mut out).is_ok());
    }

    #[test]
    fn animates_then_settles() {
        let mut out = vec![0u8; OUT_CAP];
        let spec = In { cards: vec![(1, FLAG_FOCUSED, centred(640.0, 480.0))], state: vec![], dt: 16.0, params: vec![] };
        present(&frame(&spec), &mut out).unwrap();
        assert_eq!(rd_u32(&out, 12) & 1, 1, "first frame starts from the saved rect");
        let out = settle(spec.cards);
        assert_eq!(rd_u32(&out, 12) & 1, 0);
    }

    #[test]
    fn state_follows_cards_when_their_order_changes() {
        let a = (1, FLAG_FOCUSED, centred(640.0, 480.0));
        let b = (2, 0, Rect { x: 1300.0, y: 300.0, w: 400.0, h: 300.0 });
        let settled = settle(vec![a, b]);
        let before = card_out(&settled, 0).1;
        let mut out = vec![0u8; OUT_CAP];
        present(&frame(&In { cards: vec![b, a], state: state_of(&settled), dt: 16.0, params: vec![] }), &mut out).unwrap();
        let (id, after, _) = card_out(&out, 1);
        assert_eq!(id, 1);
        assert!((after.w - before.w).abs() < 1.0, "no jump: {before:?} -> {after:?}");
    }

    #[test]
    fn rejects_bad_frames() {
        let mut out = vec![0u8; OUT_CAP];
        let mut bad = frame(&In { cards: vec![], state: vec![], dt: 16.0, params: vec![] });
        bad[0] = 0;
        assert_eq!(present(&bad, &mut out), Err(ERR_HEADER));
        let mut many = frame(&In { cards: vec![], state: vec![], dt: 16.0, params: vec![] });
        wr_u32(&mut many, 52, 999);
        assert_eq!(present(&many, &mut out), Err(ERR_COUNT));
        assert_eq!(present(&[0u8; 8], &mut out), Err(ERR_LENGTH));
    }

    #[test]
    fn garbage_numbers_stay_finite() {
        let mut out = vec![0u8; OUT_CAP];
        let spec = In { cards: vec![(1, 0, Rect { x: f32::NAN, y: f32::INFINITY, w: -5.0, h: 0.0 })], state: vec![], dt: f32::NAN, params: vec![] };
        present(&frame(&spec), &mut out).unwrap();
        let r = card_out(&out, 0).1;
        assert!(r.x.is_finite() && r.y.is_finite() && r.w.is_finite() && r.h.is_finite());
    }
}
