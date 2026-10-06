use crate::card_resize::Rect;
use crate::mini_terminal::{MIN_CARD_HEIGHT, MIN_CARD_WIDTH};

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
}
