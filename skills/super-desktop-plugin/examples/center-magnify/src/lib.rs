//! Center Magnify: a SUPER DESKTOP window renderer (renderer ABI 1).
//!
//! A card grows toward 70% of the screen width as its centre nears the
//! screen's centre and shrinks as it moves out. In the edge bands it becomes
//! an icon docked at the left or right edge. Dropping a card in an edge band
//! saves it as an icon there, so turning the plugin off keeps it an icon.
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
pub const IN_CAP: usize = IN_CARDS_AT + MAX_CARDS * IN_CARD;
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

const EDGE: f32 = 0.88; // |distance from centre| / half width beyond which a card is an icon
const FOCUSED_MAX: f32 = 0.70; // width fraction for the focused card at the centre
const OTHER_MAX: f32 = 0.45;
const MIN_FRACTION: f32 = 0.20;
const ICON: f32 = 72.0;
const GAP: f32 = 8.0;
const TAU_MS: f32 = 90.0; // smoothing time constant
const STATE_ENTRY: usize = 20; // id + x, y, w, h

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
    let n = rd_u32(input, 52) as usize;
    if n > MAX_CARDS {
        return Err(ERR_COUNT);
    }
    if input.len() < IN_CARDS_AT + n * IN_CARD {
        return Err(ERR_LENGTH);
    }
    let state_len = (rd_u32(input, 56) as usize).min(STATE_CAP);
    let state = &input[IN_HEADER..IN_HEADER + state_len];

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
    let mut drop: Option<(u32, f32, f32)> = None;

    for i in 0..n {
        let c = cards[i];
        let iconified = c.flags & FLAG_ICONIFIED != 0;
        let (cx, cy) = if iconified {
            (c.icon_x + ICON / 2.0, c.icon_y + ICON / 2.0)
        } else {
            (c.saved.x + c.saved.w / 2.0, c.saved.y + c.saved.h / 2.0)
        };
        let d = clampf(absf(cx - half) / half, 0.0, 1.0);
        let dragging = c.flags & FLAG_DRAGGING != 0;
        if (iconified || d > EDGE) && !dragging {
            mode[i] = MODE_ICON;
            if cx < half { dock_left[nl] = i; nl += 1 } else { dock_right[nr] = i; nr += 1 }
            if c.flags & FLAG_DROPPED != 0 && !iconified && drop.is_none() {
                let x = if cx < half { GAP } else { sw - ICON - GAP };
                drop = Some((c.id, x, clampf(cy - ICON / 2.0, top + GAP, sh - ICON - GAP)));
            }
            continue;
        }
        if d > EDGE {
            // Being dragged through an edge band: preview as an icon under the pointer.
            mode[i] = MODE_ICON;
            target[i] = Rect { x: cx - ICON / 2.0, y: cy - ICON / 2.0, w: ICON, h: ICON };
            continue;
        }
        let max = if c.flags & FLAG_FOCUSED != 0 { FOCUSED_MAX } else { OTHER_MAX };
        let scale = max + (MIN_FRACTION - max) * smoothstep(0.0, EDGE, d);
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
            let x = if left { GAP } else { sw - ICON - GAP };
            let y = clampf(top + GAP + k as f32 * (ICON + GAP), top, sh - ICON);
            target[i] = Rect { x, y, w: ICON, h: ICON };
        }
    }

    // Smooth from the previous frame toward the targets.
    let alpha = if dt <= 0.0 { 1.0 } else { dt / (TAU_MS + dt) };
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
        let t = target[i];
        let from = previous(i, c.id).unwrap_or(if c.flags & FLAG_ICONIFIED != 0 {
            Rect { x: c.icon_x, y: c.icon_y, w: ICON, h: ICON }
        } else {
            c.saved
        });
        let follow = c.flags & FLAG_DRAGGING != 0; // the dragged card stays under the pointer
        let mix = |a: f32, b: f32, k: f32| a + (b - a) * k;
        let r = Rect {
            x: if follow { t.x } else { mix(from.x, t.x, alpha) },
            y: if follow { t.y } else { mix(from.y, t.y, alpha) },
            w: mix(from.w, t.w, alpha),
            h: mix(from.h, t.h, alpha),
        };
        if absf(r.x - t.x) + absf(r.y - t.y) + absf(r.w - t.w) + absf(r.h - t.h) > 0.5 {
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
    if let Some((id, x, y)) = drop {
        wr_u32(out, 16, id);
        wr_f32(out, 20, x);
        wr_f32(out, 24, y);
        wr_u32(out, 28, 1);
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

    struct In {
        cards: Vec<(u32, u32, Rect)>,
        state: Vec<u8>,
        dt: f32,
    }

    fn frame(spec: &In) -> Vec<u8> {
        let mut b = vec![0u8; IN_CARDS_AT + spec.cards.len() * IN_CARD];
        wr_u32(&mut b, 0, IN_MAGIC);
        wr_u32(&mut b, 4, ABI);
        wr_f32(&mut b, 8, W);
        wr_f32(&mut b, 12, H);
        wr_f32(&mut b, 16, TOP);
        wr_f32(&mut b, 32, spec.dt);
        wr_u32(&mut b, 52, spec.cards.len() as u32);
        wr_u32(&mut b, 56, spec.state.len() as u32);
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
        let mut out = vec![0u8; OUT_CAP];
        let mut state = Vec::new();
        for _ in 0..200 {
            present(&frame(&In { cards: cards.clone(), state: state.clone(), dt: 16.0 }), &mut out).unwrap();
            state = state_of(&out);
        }
        out
    }

    fn centred(w: f32, h: f32) -> Rect {
        Rect { x: W / 2.0 - w / 2.0, y: 400.0, w, h }
    }

    #[test]
    fn focused_card_at_centre_is_seventy_percent_wide() {
        let out = settle(vec![(7, FLAG_FOCUSED, centred(640.0, 480.0))]);
        let (id, r, mode) = card_out(&out, 0);
        assert_eq!((id, mode), (7, MODE_FULL));
        assert!((r.w - 0.70 * W).abs() < 2.0 || r.h >= (H - TOP) * 0.92 - 1.0, "{r:?}");
        assert!(r.x >= 0.0 && r.x + r.w <= W + 0.5 && r.y >= TOP);
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
        let spec = In { cards: vec![(9, FLAG_DROPPED, Rect { x: 1800.0, y: 500.0, w: 200.0, h: 150.0 })], state: vec![], dt: 16.0 };
        present(&frame(&spec), &mut out).unwrap();
        assert_eq!(rd_u32(&out, 12) & 2, 2);
        assert_eq!(rd_u32(&out, 16), 9);
        assert_eq!(rd_u32(&out, 28), 1);
        assert!((rd_f32(&out, 20) - (W - ICON - GAP)).abs() < 1.0);
    }

    #[test]
    fn animates_then_settles() {
        let mut out = vec![0u8; OUT_CAP];
        let spec = In { cards: vec![(1, FLAG_FOCUSED, centred(640.0, 480.0))], state: vec![], dt: 16.0 };
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
        present(&frame(&In { cards: vec![b, a], state: state_of(&settled), dt: 16.0 }), &mut out).unwrap();
        let (id, after, _) = card_out(&out, 1);
        assert_eq!(id, 1);
        assert!((after.w - before.w).abs() < 1.0, "no jump: {before:?} -> {after:?}");
    }

    #[test]
    fn rejects_bad_frames() {
        let mut out = vec![0u8; OUT_CAP];
        let mut bad = frame(&In { cards: vec![], state: vec![], dt: 16.0 });
        bad[0] = 0;
        assert_eq!(present(&bad, &mut out), Err(ERR_HEADER));
        let mut many = frame(&In { cards: vec![], state: vec![], dt: 16.0 });
        wr_u32(&mut many, 52, 999);
        assert_eq!(present(&many, &mut out), Err(ERR_COUNT));
        assert_eq!(present(&[0u8; 8], &mut out), Err(ERR_LENGTH));
    }

    #[test]
    fn garbage_numbers_stay_finite() {
        let mut out = vec![0u8; OUT_CAP];
        let spec = In { cards: vec![(1, 0, Rect { x: f32::NAN, y: f32::INFINITY, w: -5.0, h: 0.0 })], state: vec![], dt: f32::NAN };
        present(&frame(&spec), &mut out).unwrap();
        let r = card_out(&out, 0).1;
        assert!(r.x.is_finite() && r.y.is_finite() && r.w.is_finite() && r.h.is_finite());
    }
}
