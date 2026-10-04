//! Local card geometry, checked and applied on one GTK turn.
use crate::control::{Command, Reply, Request};
use crate::desktop_protocol::{Canvas, DesktopCard, LocalWorkspaceSnapshot};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Instant;

pub struct Query {
    pub request: Request,
    pub responder: std::sync::mpsc::SyncSender<Reply>,
    pub deadline: Instant,
}

/// Journal and disk synchronization stay off the UI thread. Once queued, a
/// timeout cannot prove that the UI did not apply the operation.
pub fn dispatch(
    root: &Path,
    request: &Request,
    deadline: Instant,
    send: impl FnOnce(&Request) -> Result<Reply, ()>,
) -> Reply {
    let run = || {
        if Instant::now() >= deadline {
            return Reply::failure(
                &request.request_id,
                "timeout",
                "Request expired before application.",
            );
        }
        let reply = send(request).unwrap_or_else(|()| {
            if request.command.is_mutation() {
                Reply::unknown(&request.request_id)
            } else {
                Reply::failure(&request.request_id, "timeout", "Geometry read timed out.")
            }
        });
        if request.command.is_mutation()
            && reply.ok
            && crate::state::flush_state_saves_checked().is_err()
        {
            return Reply::unknown(&request.request_id);
        }
        reply
    };
    if request.command.is_mutation() {
        crate::control_journal::execute(root, request, |_| run())
    } else {
        run()
    }
}

pub fn card_id(command: &Command) -> &str {
    match command {
        Command::Geometry { id } | Command::Move { id, .. } | Command::Resize { id, .. } => id,
        _ => "",
    }
}

/// The opaque revision binds both the published card revision and current
/// logical output bounds. A display resize must invalidate a previous read.
pub fn revision(snapshot: &LocalWorkspaceSnapshot, card: &DesktopCard) -> String {
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(
                &card.card_id,
                &card.session_name,
                card.revision,
                &card.layout,
                card.expanded,
                &snapshot.canvas
            ))
            .unwrap()
        )
    )
}

pub fn describe(snapshot: &LocalWorkspaceSnapshot, card: &DesktopCard) -> Value {
    let layout = &card.layout;
    let (x, y, width, height) = if card.expanded {
        let (x, y, w, h) = crate::mini_terminal::expanded_rect(
            snapshot.canvas.width as i32,
            snapshot.canvas.height as i32,
        );
        (x as i32, y as i32, w as u32, h as u32)
    } else {
        (
            if layout.iconified {
                layout.icon_x.unwrap_or(layout.x)
            } else {
                layout.x
            },
            if layout.iconified {
                layout.icon_y.unwrap_or(layout.y)
            } else {
                layout.y
            },
            layout.width,
            layout.height,
        )
    };
    let (min_w, min_h) = crate::mini_terminal::min_card_size(1.0);
    json!({"id":card.card_id,"epoch":snapshot.epoch,"revision":revision(snapshot, card),
        "units":"logical-pixels","mode":if card.expanded {"expanded"} else if layout.iconified {"minimized"} else {"normal"},
        "rect":{"x":x,"y":y,"width":width,"height":height},"saved":layout,
        "canvas":snapshot.canvas,"canvasSource":"allocated-local-workspace",
        "limits":{"minWidth":min_w,"minHeight":min_h,"maxWidth":max_size(&snapshot.canvas).0,
            "maxHeight":max_size(&snapshot.canvas).1,"left":10,"top":top(&snapshot.canvas),"edge":10},
        "gridObserved":false,"gridCommand":"terminal runtime"})
}

fn top(canvas: &Canvas) -> i32 {
    70.max(canvas.top_inset as i32 + 10)
}
fn max_size(canvas: &Canvas) -> (i32, i32) {
    (
        ((canvas.width as f64 * 0.70).round() as i32).min(canvas.width as i32 - 20),
        ((canvas.height as f64 * 0.75).round() as i32).min(canvas.height as i32 - top(canvas) - 10),
    )
}

pub struct Prepared {
    pub rect: crate::card_resize::Rect,
    pub requested: Value,
    pub clamped: bool,
}

/// Validation runs immediately before widget mutation, against a fresh snapshot.
pub fn prepare(
    request: &Request,
    snapshot: &LocalWorkspaceSnapshot,
    card: &DesktopCard,
) -> Result<Prepared, Reply> {
    let fail = |code, message| Reply::failure(&request.request_id, code, message);
    let (epoch, expected, clamp) = match &request.command {
        Command::Move {
            expect_epoch,
            expect_revision,
            clamp,
            ..
        }
        | Command::Resize {
            expect_epoch,
            expect_revision,
            clamp,
            ..
        } => (expect_epoch, expect_revision, *clamp),
        _ => return Err(fail("invalid_request", "Expected a geometry mutation.")),
    };
    if epoch != &snapshot.epoch || expected != &revision(snapshot, card) {
        return Err(fail("conflict", "Geometry changed or the daemon restarted. Read terminal geometry again before deciding on a new request."));
    }
    if card.expanded {
        return Err(fail(
            "invalid_state",
            "Expanded cards cannot be moved or resized. Collapse the card first.",
        ));
    }
    let layout = &card.layout;
    let x = if layout.iconified {
        layout.icon_x.unwrap_or(layout.x)
    } else {
        layout.x
    };
    let y = if layout.iconified {
        layout.icon_y.unwrap_or(layout.y)
    } else {
        layout.y
    };
    let (mut x, mut y, mut w, mut h, requested) = match &request.command {
        Command::Move { x, y, .. } if x.abs_diff(0) <= 32768 && y.abs_diff(0) <= 32768 => (
            *x,
            *y,
            layout.width as i32,
            layout.height as i32,
            json!({"x":x,"y":y}),
        ),
        Command::Resize { width, height, .. }
            if (1..=32768).contains(width) && (1..=32768).contains(height) =>
        {
            if layout.iconified {
                return Err(fail(
                    "invalid_state",
                    "Minimized cards cannot be resized. Restore the card first.",
                ));
            }
            (
                x,
                y,
                *width as i32,
                *height as i32,
                json!({"width":width,"height":height}),
            )
        }
        _ => {
            return Err(fail(
                "invalid_arguments",
                "Coordinates must be within -32768..32768; dimensions within 1..32768.",
            ))
        }
    };
    let original = (x, y, w, h);
    if matches!(request.command, Command::Resize { .. }) {
        let (min_w, min_h) = crate::mini_terminal::min_card_size(1.0);
        let (max_w, max_h) = max_size(&snapshot.canvas);
        if max_w < min_w || max_h < min_h {
            return Err(fail(
                "out_of_bounds",
                "The current logical display is too small for a normal card.",
            ));
        }
        w = w.clamp(min_w, max_w);
        h = h.clamp(min_h, max_h);
    }
    let max_x = snapshot.canvas.width as i32 - w - 10;
    let max_y = snapshot.canvas.height as i32 - h - 10;
    if max_x < 10 || max_y < top(&snapshot.canvas) {
        return Err(fail(
            "out_of_bounds",
            "The whole card does not fit the current logical display. Resize it first.",
        ));
    }
    x = x.clamp(10, max_x);
    y = y.clamp(top(&snapshot.canvas), max_y);
    let clamped = original != (x, y, w, h);
    if clamped && !clamp {
        return Err(fail("out_of_bounds", "Requested geometry exceeds current bounds. Read geometry limits or explicitly pass --clamp."));
    }
    Ok(Prepared {
        rect: crate::card_resize::Rect {
            x: x as f64,
            y: y as f64,
            width: w,
            height: h,
        },
        requested,
        clamped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> LocalWorkspaceSnapshot {
        serde_json::from_value(json!({"epoch":"epoch-1","revision":7,
            "canvas":{"x":0,"y":0,"width":1024,"height":768,"scale":1.0,"topInset":60},
            "workspace":"/tmp","homeDirectory":"/tmp","visibleHarnesses":[],"harnessTypes":[],
            "cards":[{"cardId":"sd_term_test","sessionName":"sd_term_test","agentType":"shell",
                "title":"private prompt","status":"UNKNOWN","sessionAlive":null,"workspace":"/tmp","revision":7,
                "layout":{"x":20,"y":80,"width":480,"height":320,"restoredWidth":480,"restoredHeight":320,
                    "iconified":false,"iconX":null,"iconY":null,"tag":0},"stackingOrder":0,"expanded":false,"terminalSize":null}]})).unwrap()
    }
    fn request(s: &LocalWorkspaceSnapshot, resize: bool, clamp: bool) -> Request {
        let card = &s.cards[0];
        Request {
            control_version: 1,
            request_id: "geometry-test".into(),
            command: if resize {
                Command::Resize {
                    id: card.card_id.clone(),
                    width: 640,
                    height: 480,
                    clamp,
                    expect_epoch: s.epoch.clone(),
                    expect_revision: revision(s, card),
                }
            } else {
                Command::Move {
                    id: card.card_id.clone(),
                    x: 80,
                    y: 100,
                    clamp,
                    expect_epoch: s.epoch.clone(),
                    expect_revision: revision(s, card),
                }
            },
        }
    }
    #[test]
    fn cli_geometry_stale_card_epoch_and_logical_output_are_refused() {
        let s = snapshot();
        let r = request(&s, false, false);
        for mutate in [
            |s: &mut LocalWorkspaceSnapshot| s.cards[0].revision += 1,
            |s: &mut LocalWorkspaceSnapshot| s.cards[0].card_id = "different-card".into(),
            |s: &mut LocalWorkspaceSnapshot| s.epoch = "restart".into(),
            |s: &mut LocalWorkspaceSnapshot| s.canvas.width = 800,
            |s: &mut LocalWorkspaceSnapshot| s.canvas.scale = 2.0,
            |s: &mut LocalWorkspaceSnapshot| s.canvas.top_inset = 90,
        ] {
            let mut changed = s.clone();
            mutate(&mut changed);
            let error = prepare(&r, &changed, &changed.cards[0]).err().unwrap();
            assert_eq!(error.exit_code(), 5);
            assert_eq!(error.error.unwrap().outcome, "not_applied");
        }
    }
    #[test]
    fn cli_geometry_bounds_require_opt_in_and_use_whole_card() {
        let s = snapshot();
        let mut r = request(&s, false, false);
        if let Command::Move { x, y, .. } = &mut r.command {
            *x = 1000;
            *y = -20;
        }
        assert_eq!(
            prepare(&r, &s, &s.cards[0])
                .err()
                .unwrap()
                .error
                .unwrap()
                .code,
            "out_of_bounds"
        );
        if let Command::Move { clamp, .. } = &mut r.command {
            *clamp = true;
        }
        let p = prepare(&r, &s, &s.cards[0]).unwrap_or_else(|_| panic!("clamp"));
        assert_eq!(
            (p.rect.x, p.rect.y, p.rect.width, p.rect.height),
            (534.0, 70.0, 480, 320)
        );
        assert!(p.clamped);
        assert_eq!(p.requested, json!({"x":1000,"y":-20}));
        let mut r = request(&s, true, true);
        if let Command::Resize { width, height, .. } = &mut r.command {
            *width = 32768;
            *height = 32768;
        }
        let p = prepare(&r, &s, &s.cards[0]).unwrap_or_else(|_| panic!("resize clamp"));
        assert_eq!((p.rect.width, p.rect.height), (717, 576));
        if let Command::Resize { width, .. } = &mut r.command {
            *width = u32::MAX;
        }
        assert_eq!(prepare(&r, &s, &s.cards[0]).err().unwrap().exit_code(), 2);
    }
    #[test]
    fn cli_geometry_modes_and_small_outputs_fail_without_restoring_or_resizing() {
        let mut s = snapshot();
        s.cards[0].expanded = true;
        for resize in [false, true] {
            assert_eq!(
                prepare(&request(&s, resize, true), &s, &s.cards[0])
                    .err()
                    .unwrap()
                    .error
                    .unwrap()
                    .code,
                "invalid_state"
            );
        }
        s.cards[0].expanded = false;
        s.cards[0].layout.iconified = true;
        s.cards[0].layout.width = 80;
        s.cards[0].layout.height = 80;
        assert!(prepare(&request(&s, false, false), &s, &s.cards[0]).is_ok());
        assert!(prepare(&request(&s, true, true), &s, &s.cards[0]).is_err());
        s.cards[0].layout.iconified = false;
        s.canvas.width = 100;
        assert_eq!(
            prepare(&request(&s, true, true), &s, &s.cards[0])
                .err()
                .unwrap()
                .error
                .unwrap()
                .code,
            "out_of_bounds"
        );
    }
    #[test]
    fn cli_geometry_resize_clamps_origin_only_with_opt_in_and_never_changes_units() {
        let mut s = snapshot();
        s.canvas.scale = 2.0;
        s.cards[0].layout.x = 450;
        assert!(prepare(&request(&s, true, false), &s, &s.cards[0]).is_err());
        let p =
            prepare(&request(&s, true, true), &s, &s.cards[0]).unwrap_or_else(|_| panic!("clamp"));
        assert_eq!((p.rect.x, p.rect.width), (374.0, 640));
        let info = describe(&s, &s.cards[0]);
        assert_eq!(info["units"], "logical-pixels");
        assert_eq!(info["canvas"]["scale"], 2.0);
        assert_eq!(info["gridObserved"], false);
        assert!(!info.to_string().contains("private prompt"));
    }
    #[test]
    fn cli_geometry_receipt_deduplicates_target_and_preserves_unknown_queue_outcomes() {
        let root = std::env::temp_dir().join(format!(
            "sd-geometry-{}",
            crate::control::new_request(Command::Status {})
                .unwrap()
                .request_id
        ));
        let s = snapshot();
        let r = request(&s, false, false);
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let reply = dispatch(&root, &r, deadline, |_| {
            Ok(Reply::success(&r.request_id, json!({"outcome":"applied"})))
        });
        assert!(reply.ok);
        assert!(dispatch(&root, &r, deadline, |_| panic!("must not reapply")).ok);
        let receipt = crate::control_journal::inspect(&root, "read", &r.request_id)
            .data
            .unwrap();
        assert_eq!(receipt["cardId"], "sd_term_test");
        let mut second = r.clone();
        second.request_id = "lost-reply".into();
        assert_eq!(
            dispatch(&root, &second, deadline, |_| Err(()))
                .error
                .unwrap()
                .outcome,
            "unknown"
        );
        assert_eq!(
            dispatch(&root, &second, deadline, |_| panic!(
                "must not replay unknown"
            ))
            .error
            .unwrap()
            .outcome,
            "unknown"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
