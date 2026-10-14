use crate::card_resize::Rect;
use crate::mini_terminal::{workspace_limits, MIN_CARD_HEIGHT, MIN_CARD_WIDTH};

/// Fill the usable workspace, favoring readable landscape terminals. An
/// incomplete final row stretches across the full width instead of leaving holes.
pub fn terminal_grid(area: Rect, count: usize) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    let gap = 12.min((area.width - 1).max(0) / count as i32)
        .min((area.height - 1).max(0) / count as i32);
    let rows = (1..=count).min_by(|&a, &b| {
        let score = |rows: usize| {
            let columns = count.div_ceil(rows);
            let w = (area.width - gap * (columns as i32 - 1)) as f64 / columns as f64;
            let h = (area.height - gap * (rows as i32 - 1)) as f64 / rows as f64;
            let undersize = (f64::from(MIN_CARD_WIDTH) / w.max(1.0)).max(1.0)
                * (f64::from(MIN_CARD_HEIGHT) / h.max(1.0)).max(1.0);
            undersize * 100.0 + ((w.max(1.0) / h.max(1.0)) / 1.6).ln().abs()
        };
        score(a).total_cmp(&score(b))
    }).unwrap();
    let columns = count.div_ceil(rows);
    let rows = count.div_ceil(columns);
    let usable_h = (area.height - gap * (rows as i32 - 1)).max(rows as i32);
    let mut result = Vec::with_capacity(count);
    for row in 0..rows {
        let in_row = columns.min(count - result.len());
        let usable_w = (area.width - gap * (in_row as i32 - 1)).max(in_row as i32);
        let y = row as i32 * usable_h / rows as i32;
        let bottom = (row + 1) as i32 * usable_h / rows as i32;
        for column in 0..in_row {
            let x = column as i32 * usable_w / in_row as i32;
            let right = (column + 1) as i32 * usable_w / in_row as i32;
            result.push(Rect {
                x: area.x + f64::from(x + column as i32 * gap),
                y: area.y + f64::from(y + row as i32 * gap),
                width: right - x, height: bottom - y,
            });
        }
    }
    result
}

/// Where a card at `rect` goes when the workspace changes from `from` to `to`
/// (another display, or a new scale): it keeps the same share of the room it
/// had to move in, so a card at an edge stays at that edge and a centred card
/// stays centred, and all of it ends up inside the margins of
/// `workspace_limits`. A card larger than the new workspace shrinks to fit it,
/// never below `min`; a smaller one keeps its size. Between equal sizes a card
/// on screen stays where it is and one hanging past an edge comes back in.
pub fn follow_workspace(rect: Rect, from: (i32, i32), to: (i32, i32), min: (i32, i32)) -> Rect {
    let old = workspace_limits(from, 1.0);
    let new = workspace_limits(to, 1.0);
    let (x, width) = follow_axis(rect.x, rect.width, (old.left, old.right), (new.left, new.right), min.0);
    let (y, height) = follow_axis(rect.y, rect.height, (old.top, old.bottom), (new.top, new.bottom), min.1);
    Rect { x, y, width, height }
}

/// One axis of [`follow_workspace`]: `(start, end)` are the edges a card stays
/// between.
fn follow_axis(pos: f64, len: i32, (old_start, old_end): (f64, f64), (start, end): (f64, f64), min: i32) -> (f64, i32) {
    let old_room = old_end - old_start - f64::from(len);
    let share = if old_room > 0.0 {
        ((pos - old_start) / old_room).clamp(0.0, 1.0)
    } else if pos > old_start {
        1.0
    } else {
        0.0
    };
    let fits = (end - start).floor() as i32;
    let len = len.min(fits.max(min));
    let room = (end - start - f64::from(len)).max(0.0);
    ((start + share * room).round(), len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrange_grid_keeps_terminals_readable_when_the_display_has_room() {
        let area = Rect { x: 12.0, y: 58.0, width: 1896, height: 1010 };
        assert_eq!(terminal_grid(area, 1), vec![area]);
        for count in 2..=12 {
            for rect in terminal_grid(area, count) {
                assert!(rect.width >= MIN_CARD_WIDTH);
                assert!(rect.height >= MIN_CARD_HEIGHT);
            }
        }
    }

    #[test]
    fn arrange_grid_fills_display_without_overlap_after_repeated_changes() {
        for (width, height) in [(1920, 1000), (640, 420), (3440, 1300), (800, 1100), (640, 420)] {
            for count in 1..=12 {
                let area = Rect { x: 12.0, y: 76.0, width, height };
                let rects = terminal_grid(area, count);
                assert_eq!(rects.len(), count);
                for (i, r) in rects.iter().enumerate() {
                    assert!(r.width > 0 && r.height > 0);
                    assert!(r.x >= area.x && r.y >= area.y);
                    assert!(r.x + f64::from(r.width) <= area.x + f64::from(width));
                    assert!(r.y + f64::from(r.height) <= area.y + f64::from(height));
                    for other in &rects[..i] {
                        assert!(r.x >= other.x + f64::from(other.width)
                            || other.x >= r.x + f64::from(r.width)
                            || r.y >= other.y + f64::from(other.height)
                            || other.y >= r.y + f64::from(r.height));
                    }
                }
                let last = rects.last().unwrap();
                assert_eq!(last.x + f64::from(last.width), area.x + f64::from(width));
                assert_eq!(last.y + f64::from(last.height), area.y + f64::from(height));
            }
        }
        assert!(terminal_grid(Rect { x: 0.0, y: 0.0, width: 800, height: 600 }, 0).is_empty());
    }

    const MIN: (i32, i32) = (MIN_CARD_WIDTH, MIN_CARD_HEIGHT);
    const WIDE: (i32, i32) = (3440, 1440);
    const LAPTOP: (i32, i32) = (1536, 960);

    fn card(x: f64, y: f64, width: i32, height: i32) -> Rect {
        Rect { x, y, width, height }
    }

    fn inside(rect: Rect, (width, height): (i32, i32)) -> bool {
        rect.x >= 10.0 && rect.y >= 70.0
            && rect.x + f64::from(rect.width) <= f64::from(width - 10)
            && rect.y + f64::from(rect.height) <= f64::from(height - 10)
    }

    #[test]
    fn arrange_follow_keeps_edges_and_centres_on_another_display() {
        // Right and bottom edges of the wide display stay the edges.
        let corner = follow_workspace(card(2790.0, 950.0, 640, 480), WIDE, LAPTOP, MIN);
        assert_eq!(corner, card(886.0, 470.0, 640, 480));
        // The left and top ones too.
        assert_eq!(follow_workspace(card(10.0, 70.0, 640, 480), WIDE, LAPTOP, MIN), card(10.0, 70.0, 640, 480));
        // A centred card stays centred.
        let centred = follow_workspace(card(1400.0, 510.0, 640, 480), WIDE, (1920, 1080), MIN);
        assert_eq!(centred, card(640.0, 330.0, 640, 480));
        // Back on the wide display the cards return to their places.
        assert_eq!(follow_workspace(corner, LAPTOP, WIDE, MIN), card(2790.0, 950.0, 640, 480));
        assert_eq!(follow_workspace(centred, (1920, 1080), WIDE, MIN), card(1400.0, 510.0, 640, 480));
    }

    #[test]
    fn arrange_follow_never_leaves_a_card_off_the_new_display() {
        let cards = [
            card(2900.0, 1200.0, 640, 480),
            card(-400.0, -50.0, 800, 600),
            card(5000.0, 3000.0, 320, 180),
            card(1200.0, 300.0, 3000, 1300),
            card(30.0, 900.0, 128, 128),
        ];
        let sizes = [WIDE, LAPTOP, (1024, 768), (2560, 1600), (800, 600), WIDE];
        for card in cards {
            let mut moved = card;
            for pair in sizes.windows(2) {
                moved = follow_workspace(moved, pair[0], pair[1], MIN);
                assert!(inside(moved, pair[1]), "{card:?} lands off the {:?} display at {moved:?}", pair[1]);
            }
        }
    }

    #[test]
    fn arrange_follow_shrinks_only_cards_too_big_for_the_new_display() {
        let big = follow_workspace(card(10.0, 70.0, 3000, 1300), WIDE, LAPTOP, MIN);
        assert_eq!(big, card(10.0, 70.0, 1516, 880));
        // A small card (an icon) keeps its size, even below the minimum.
        assert_eq!(follow_workspace(card(10.0, 70.0, 128, 128), WIDE, LAPTOP, MIN).width, 128);
        // A display smaller than the minimum card keeps the minimum.
        let tiny = follow_workspace(card(10.0, 70.0, 640, 480), WIDE, (200, 150), MIN);
        assert_eq!((tiny.x, tiny.y, tiny.width, tiny.height), (10.0, 70.0, MIN_CARD_WIDTH, MIN_CARD_HEIGHT));
    }

    #[test]
    fn arrange_follow_between_equal_sizes_only_brings_cards_back_on_screen() {
        let on_screen = card(700.0, 300.0, 640, 480);
        assert_eq!(follow_workspace(on_screen, LAPTOP, LAPTOP, MIN), on_screen);
        let past_right = follow_workspace(card(1400.0, 300.0, 640, 480), LAPTOP, LAPTOP, MIN);
        assert_eq!(past_right, card(886.0, 300.0, 640, 480));
    }
}
