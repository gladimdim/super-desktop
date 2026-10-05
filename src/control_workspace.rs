//! Local workspace validation. Wire contracts of the remote bridge are unchanged.
use crate::control::{Reply, Request};
use crate::desktop_protocol::{Canvas, LocalWorkspaceSnapshot};
use crate::state::{AppState, NoteData};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub fn revision(state: &AppState, snapshot: &LocalWorkspaceSnapshot) -> String {
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(
                state,
                &snapshot.canvas,
                snapshot
                    .cards
                    .iter()
                    .map(|c| (&c.card_id, c.expanded))
                    .collect::<Vec<_>>()
            ))
            .unwrap()
        )
    )
}

pub fn note(data: &NoteData, content: bool) -> Value {
    let mut value = json!({"id":data.id,"rect":{"x":data.x,"y":data.y,"width":data.width,"height":data.height},
        "tag":data.tag,"color":data.color,"updatedAt":data.updated_at,"textBytes":data.text.len()});
    if content {
        value["text"] = json!(data.text);
    }
    value
}

pub fn validate_note(data: &NoteData, canvas: &Canvas, geometry: bool) -> Result<(), &'static str> {
    if data.text.len() > 4096
        || data
            .text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
        return Err("Notes accept at most 4096 UTF-8 bytes; only newline/tab control characters are allowed.");
    }
    if data.tag > 8 {
        return Err("Tag must be 0..8.");
    }
    if geometry {
        let top = 70.max(canvas.top_inset as i32 + 10);
        let w = canvas.width as i64;
        let h = canvas.height as i64;
        if data.width < crate::sticky_note::MIN_NOTE_WIDTH
            || data.height < crate::sticky_note::MIN_NOTE_HEIGHT
            || data.width as i64 > (w as f64 * 0.70).round() as i64
            || data.height as i64 > (h as f64 * 0.75).round() as i64
            || data.x < 10
            || data.y < top
            || data.x as i64 + data.width as i64 > w - 10
            || data.y as i64 + data.height as i64 > h - 10
        {
            return Err("Note rectangle must fit the current logical canvas; minimum 180x120, maximum 70% width and 75% height, with toolbar and 10px edges reserved.");
        }
    }
    Ok(())
}

pub fn check(
    request: &Request,
    state: &AppState,
    snapshot: &LocalWorkspaceSnapshot,
    epoch: &str,
) -> Result<(), Reply> {
    let (expect_epoch, expect_revision) = match &request.command {
        crate::control::Command::WorkspaceEdit {
            expect_epoch,
            expect_revision,
            ..
        }
        | crate::control::Command::PreferencesEdit {
            expect_epoch,
            expect_revision,
            ..
        } => (expect_epoch, expect_revision),
        _ => {
            return Err(Reply::failure(
                &request.request_id,
                "invalid_request",
                "Expected a guarded local edit.",
            ))
        }
    };
    if expect_epoch != epoch || expect_revision != &revision(state, snapshot) {
        return Err(Reply::failure(&request.request_id,"conflict","Workspace, note editor or display changed. Read workspace inspect or note inspect again."));
    }
    Ok(())
}

pub fn export(state: &AppState, snapshot: &LocalWorkspaceSnapshot) -> crate::control::Layout {
    use crate::control::LayoutItem;
    let mut items = state
        .notes
        .iter()
        .map(|n| LayoutItem {
            kind: "note".into(),
            id: n.id.clone(),
            mode: "normal".into(),
            x: n.x,
            y: n.y,
            width: n.width,
            height: n.height,
        })
        .collect::<Vec<_>>();
    for card in &snapshot.cards {
        let d = crate::control_geometry::describe(snapshot, card);
        let rect = &d["rect"];
        items.push(LayoutItem {
            kind: "terminal".into(),
            id: card.card_id.clone(),
            mode: d["mode"].as_str().unwrap().into(),
            x: rect["x"].as_i64().unwrap() as i32,
            y: rect["y"].as_i64().unwrap() as i32,
            width: rect["width"].as_i64().unwrap() as i32,
            height: rect["height"].as_i64().unwrap() as i32,
        });
    }
    crate::control::Layout { version: 1, items }
}

pub fn validate_layout(
    layout: &crate::control::Layout,
    state: &AppState,
    snapshot: &LocalWorkspaceSnapshot,
) -> Result<(), &'static str> {
    if layout.version != 1 || layout.items.len() > 64 {
        return Err("Use layout version 1 with at most 64 items per operation.");
    }
    let mut seen = std::collections::HashSet::new();
    for item in &layout.items {
        if !seen.insert((&item.kind, &item.id)) {
            return Err("Repeated layout item ID.");
        }
        if item.kind == "note" {
            let mut notes = state.notes.iter().filter(|n| n.id == item.id);
            let mut note = notes.next().ok_or("Unknown note ID.")?.clone();
            if notes.next().is_some() || item.mode != "normal" {
                return Err("Ambiguous note or invalid mode.");
            }
            // Layout never reads or modifies note content.
            note.text.clear();
            note.x = item.x;
            note.y = item.y;
            note.width = item.width;
            note.height = item.height;
            validate_note(&note, &snapshot.canvas, true)?;
        } else if item.kind == "terminal" {
            let mut cards = snapshot.cards.iter().filter(|c| c.card_id == item.id);
            let card = cards.next().ok_or("Unknown terminal ID.")?;
            if cards.next().is_some()
                || card.expanded
                || item.mode
                    != if card.layout.iconified {
                        "minimized"
                    } else {
                        "normal"
                    }
            {
                return Err("Terminal mode changed or is expanded. Set card modes separately before applying a layout.");
            }
            let d = crate::control_geometry::describe(snapshot, card);
            if item.width <= 0
                || item.height <= 0
                || item.x < 10
                || item.y < (d["limits"]["top"].as_i64().unwrap() as i32)
                || item.x as i64 + item.width as i64 > snapshot.canvas.width as i64 - 10
                || item.y as i64 + item.height as i64 > snapshot.canvas.height as i64 - 10
            {
                return Err("Terminal rectangle does not fit the current logical canvas.");
            }
            if card.layout.iconified {
                if item.width as u32 != card.layout.width
                    || item.height as u32 != card.layout.height
                {
                    return Err("A minimized card can move but cannot resize.");
                }
            } else if (item.width as i64) < d["limits"]["minWidth"].as_i64().unwrap()
                || (item.height as i64) < d["limits"]["minHeight"].as_i64().unwrap()
                || item.width as i64 > d["limits"]["maxWidth"].as_i64().unwrap()
                || item.height as i64 > d["limits"]["maxHeight"].as_i64().unwrap()
            {
                return Err("Terminal dimensions exceed current normal-card limits.");
            }
        } else {
            return Err(
                "Layout kinds are note or terminal; commands and launch data are never imported.",
            );
        }
    }
    Ok(())
}

pub fn arrange(
    state: &AppState,
    snapshot: &LocalWorkspaceSnapshot,
) -> Result<crate::control::Layout, &'static str> {
    let mut layout = export(state, snapshot);
    if layout.items.len() > 64
        || layout
            .items
            .iter()
            .any(|i| !(1..=32768).contains(&i.width) || !(1..=32768).contains(&i.height))
    {
        return Err("Arrange supports up to 64 cards with bounded dimensions.");
    }
    layout
        .items
        .sort_by(|a, b| (&a.kind, &a.id).cmp(&(&b.kind, &b.id)));
    let left = 10;
    let top = 70.max(snapshot.canvas.top_inset as i32 + 10);
    let (mut x, mut y, mut row_height) = (left, top, 0);
    for item in &mut layout.items {
        if x as i64 + item.width as i64 > snapshot.canvas.width as i64 - 10 {
            x = left;
            y += row_height + 10;
            row_height = 0;
        }
        item.x = x;
        item.y = y;
        x = x
            .checked_add(item.width)
            .and_then(|n| n.checked_add(10))
            .ok_or("Layout is too large.")?;
        row_height = row_height.max(item.height);
    }
    validate_layout(&layout, state, snapshot)?;
    Ok(layout)
}
