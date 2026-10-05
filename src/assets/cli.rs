//! Separate local CLI catalog; existing desktop and bridge catalogs are unchanged.
use super::*;
use crate::state::TerminalData;
static CLI: OnceLock<Mutex<Catalog>> = OnceLock::new();
fn store() -> &'static Mutex<Catalog> {
    CLI.get_or_init(Mutex::default)
}
fn root(card: &TerminalData) -> Result<PathBuf, String> {
    let path = card
        .workspace_dir
        .as_ref()
        .ok_or("workspace_not_recorded")?;
    let root = std::fs::canonicalize(path).map_err(|_| "workspace_unavailable")?;
    if root == Path::new("/") {
        return Err("root_workspace_not_shared".into());
    }
    Ok(root)
}
fn remember(card: &TerminalData, entries: Vec<Entry>) -> Vec<Asset> {
    let mut catalog = store().lock().unwrap();
    catalog.sessions.retain(|(id, _)| id != &card.id);
    let result = entries.iter().map(|e| e.asset.clone()).collect();
    catalog.sessions.push_back((card.id.clone(), entries));
    while catalog.sessions.len() > MAX_SESSIONS {
        catalog.sessions.pop_front();
    }
    result
}
pub(crate) fn list(card: &TerminalData, screen: &str) -> Result<Vec<Asset>, String> {
    let root = root(card)?;
    let mut paths = crate::asset_references::candidates(screen, |path| {
        desktop_file_type(Path::new(path)).is_some()
    });
    let mut history = crate::asset_history::merge(&card.session_name, &root, vec![])
        .map_err(|_| "reference_history_unavailable")?;
    paths.append(&mut history);
    let mut seen = std::collections::HashSet::new();
    let mut entries = vec![];
    for path in paths {
        if let Ok(entry) = register_for(&root, &card.session_name, &path, true) {
            if seen.insert(entry.relative.clone()) {
                entries.push(entry);
            }
        }
        if entries.len() >= MAX_ASSETS {
            break;
        }
    }
    Ok(remember(card, entries))
}
pub(crate) fn add(card: &TerminalData, path: &str) -> Result<Asset, String> {
    let root = root(card)?;
    let entry = register_for(&root, &card.session_name, path, true)?;
    crate::asset_history::merge(
        &card.session_name,
        &root,
        vec![entry.asset.relative_path.clone()],
    )
    .map_err(|_| "reference_history_unavailable")?;
    let catalog = store().lock().unwrap();
    let mut entries = catalog
        .sessions
        .iter()
        .find(|(id, _)| id == &card.id)
        .map(|(_, e)| e.clone())
        .unwrap_or_default();
    drop(catalog);
    entries.retain(|e| e.root == root && e.relative != entry.relative);
    entries.insert(0, entry.clone());
    entries.truncate(MAX_ASSETS);
    remember(card, entries);
    Ok(entry.asset)
}
fn listed(card: &TerminalData, id: &str) -> Result<Entry, String> {
    let root = root(card)?;
    store()
        .lock()
        .unwrap()
        .sessions
        .iter()
        .find(|(key, _)| key == &card.id)
        .and_then(|(_, entries)| entries.iter().find(|e| e.asset.id == id && e.root == root))
        .cloned()
        .ok_or("asset_expired_refresh_list".into())
}
pub(crate) fn read(card: &TerminalData, id: &str) -> Result<(Asset, Vec<u8>), String> {
    read_entry(&listed(card, id)?)
}
pub(crate) fn save(card: &TerminalData, id: &str, text: &str) -> Result<Asset, String> {
    let entry = listed(card, id)?;
    let saved = write_entry(&card.session_name, &entry, text).map_err(|e| match e.as_str() {
        "only_markdown_is_editable"
        | "file_too_large"
        | "file_content_does_not_match_type"
        | "file_changed_on_disk_refresh_list" => e,
        _ => "write_failed".into(),
    })?;
    let mut catalog = store().lock().unwrap();
    if let Some(slot) = catalog
        .sessions
        .iter_mut()
        .find(|(key, _)| key == &card.id)
        .and_then(|(_, entries)| entries.iter_mut().find(|e| e.asset.id == id))
    {
        *slot = saved.clone();
    }
    Ok(saved.asset)
}
pub(crate) fn remove(card: &TerminalData, id: &str) -> Result<Asset, String> {
    let entry = listed(card, id)?;
    crate::asset_history::remove(&card.session_name, &entry.root, &entry.asset.relative_path)
        .map_err(|_| "reference_history_unavailable")?;
    if let Some((_, entries)) = store()
        .lock()
        .unwrap()
        .sessions
        .iter_mut()
        .find(|(key, _)| key == &card.id)
    {
        entries.retain(|e| e.asset.id != id);
    }
    Ok(entry.asset)
}
