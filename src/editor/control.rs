//! Operate on live buffers by their per-open identity, including unsaved text.
use super::*;
use crate::control::EditorAction;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn editor(id: &str) -> Result<Rc<Editor>, &'static str> {
    editors()
        .into_iter()
        .find(|e| e.id == id)
        .ok_or("Unknown editor card.")
}
fn doc(e: &Editor, id: &str) -> Result<Rc<Doc>, &'static str> {
    e.docs
        .borrow()
        .iter()
        .find(|d| d.id == id)
        .cloned()
        .ok_or("The document is no longer open.")
}
fn revision(d: &Doc, text: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(d.id.as_bytes());
    hash.update([d.is_modified() as u8, d.stale.get() as u8]);
    hash.update(text.as_bytes());
    format!("{:x}", hash.finalize())
}
pub(super) fn inventory() -> Value {
    json!(editors().iter().map(|e|{
        let mut info=json!(e.saved());
        info["documents"]=json!(e.docs_in_tab_order().iter().map(|d|json!({"id":d.id,"path":*d.path.borrow(),"title":d.title(),"modified":d.is_modified()})).collect::<Vec<_>>());
        info
    }).collect::<Vec<_>>())
}
pub(crate) fn read(id: &str, document: &str, offset: u32) -> Result<Value, &'static str> {
    let e = editor(id)?;
    let d = doc(&e, document)?;
    let text = d.text();
    let start = offset as usize;
    if !text.is_char_boundary(start) {
        return Err("Offset must be a UTF-8 boundary inside the buffer.");
    }
    let mut end = (start + 8192).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Ok(
        json!({"editor":id,"document":document,"text":&text[start..end],"bytes":text.len(),"offset":start,
        "nextOffset":(end<text.len()).then_some(end),"bufferRevision":revision(&d,&text),"modified":d.is_modified(),"stale":d.stale.get()}),
    )
}

pub(crate) fn edit(id: &str, action: &EditorAction) -> Result<Value, String> {
    let editor = editor(id)?;
    if editor.prompt.answer.borrow().is_some() {
        return Err("Resolve the open editor question first.".into());
    }
    match action {
        EditorAction::New => {
            editor.new_file();
            return Ok(json!({"document":editor.current().unwrap().id}));
        }
        EditorAction::CloseCard => {
            if editor.docs.borrow().iter().any(|d| d.is_modified()) {
                return Err("Save the modified documents before closing the card.".into());
            }
            editor.close_now();
            return Ok(json!({"closed":true}));
        }
        _ => {}
    }
    let (document, expected) = match action {
        EditorAction::Splice {
            document, revision, ..
        }
        | EditorAction::Save { document, revision }
        | EditorAction::Close { document, revision }
        | EditorAction::SaveNew {
            document, revision, ..
        } => (document, revision),
        _ => unreachable!(),
    };
    let d = doc(&editor, document)?;
    let current = d.text();
    if revision(&d, &current) != *expected || d.stale.get() {
        return Err("The buffer or disk copy changed. Read the document again.".into());
    }
    match action {
        EditorAction::Splice {
            start, end, text, ..
        } => {
            let (start, end) = (*start as usize, *end as usize);
            if start > end
                || !current.is_char_boundary(start)
                || !current.is_char_boundary(end)
                || text.contains('\0')
                || text.len() > 8192
                || current.len() - (end - start) + text.len() > doc::MAX_BYTES
            {
                return Err("Invalid UTF-8 range or editor text limit.".into());
            }
            let mut a = d
                .buffer
                .iter_at_offset(current[..start].chars().count() as i32);
            let mut b = d
                .buffer
                .iter_at_offset(current[..end].chars().count() as i32);
            d.buffer.begin_user_action();
            d.buffer.delete(&mut a, &mut b);
            d.buffer.insert(&mut a, text);
            d.buffer.end_user_action();
        }
        EditorAction::Save { .. } => d.save()?,
        EditorAction::SaveNew { path, .. } => {
            let path = Path::new(path);
            if !path.is_absolute() {
                return Err("Use an absolute new path.".into());
            }
            d.save_new(path)?;
            persist();
        }
        EditorAction::Close { .. } => {
            if d.is_modified() {
                return Err("Save the modified document before closing it.".into());
            }
            editor.remove_doc(&d);
            return Ok(json!({"closed":true}));
        }
        _ => unreachable!(),
    }
    Ok(json!({"document":d.id,"modified":d.is_modified(),"bufferRevision":revision(&d,&d.text())}))
}
