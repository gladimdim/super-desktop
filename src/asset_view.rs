//! Lazy file drawer. All filesystem work and decoding stays off GTK's thread.
use gtk4::gdk_pixbuf;
use gtk4::{gdk, gio, glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

struct Frame {
    pixels: Vec<u8>,
    width: i32,
    height: i32,
    stride: usize,
    alpha: bool,
    delay: Duration,
}
enum Preview {
    Text(String),
    /// The whole file, so it can be edited and saved back.
    Markdown(String),
    Images(Vec<Frame>),
    Audio(Vec<u8>),
}

fn frames(bytes: &[u8]) -> Result<Vec<Frame>, String> {
    let count = if bytes.starts_with(b"GIF") {
        crate::assets::gif_frames(bytes)?
    } else {
        1
    };
    let too_big = Rc::new(Cell::new(false));
    let loader = gdk_pixbuf::PixbufLoader::new();
    let flag = Rc::clone(&too_big);
    loader.connect_size_prepared(move |loader, width, height| {
        if width <= 0 || height <= 0 || i64::from(width) * i64::from(height) > 16_000_000 {
            flag.set(true);
            loader.set_size(1, 1);
        } else if width.max(height) > 1600 {
            let scale = 1600.0 / f64::from(width.max(height));
            loader.set_size(
                (width as f64 * scale).max(1.0) as i32,
                (height as f64 * scale).max(1.0) as i32,
            );
        }
    });
    let written = loader.write(bytes);
    let closed = loader.close();
    let decoded = written.and(closed);
    if decoded.is_err() || too_big.get() {
        return Err("Image cannot be decoded or is larger than 16 megapixels".into());
    }
    let animation = loader.animation().ok_or("Image has no frames")?;
    let mut now = SystemTime::now();
    let iter = animation.iter(Some(now));
    let mut result = Vec::new();
    for _ in 0..count {
        let pixbuf = iter.pixbuf();
        let delay = iter
            .delay_time()
            .unwrap_or(Duration::from_secs(1))
            .max(Duration::from_millis(30));
        result.push(Frame {
            pixels: pixbuf.read_pixel_bytes().to_vec(),
            width: pixbuf.width(),
            height: pixbuf.height(),
            stride: pixbuf.rowstride() as usize,
            alpha: pixbuf.has_alpha(),
            delay,
        });
        now += delay;
        iter.advance(now);
    }
    Ok(result)
}

fn audio_preview(bytes: Vec<u8>) -> gtk4::Box {
    let bytes = glib::Bytes::from_owned(bytes);
    let media = gtk4::MediaFile::new();
    let panel = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    let message = gtk4::Label::new(Some("Press Play to listen."));
    message.set_wrap(true);
    let update = |media: &gtk4::MediaFile, label: &gtk4::Label| {
        if let Some(error) = media.error() {
            label.set_text(&format!("Audio playback unavailable: {error}"));
        } else {
            label.set_text("Press Play to listen.");
        }
    };
    update(&media, &message);
    media.connect_error_notify({
        let message = message.downgrade();
        move |media| { if let Some(label) = message.upgrade() { update(media, &label); } }
    });
    panel.append(&message);
    panel.append(&gtk4::MediaControls::new(Some(&media)));
    // Removing the preview, closing Files or hiding the overlay stops playback.
    panel.connect_map({
        let media = media.clone();
        move |_| {
            let stream = gio::MemoryInputStream::from_bytes(&bytes);
            media.set_input_stream(Some(&stream));
        }
    });
    panel.connect_unmap(move |_| {
        media.pause();
        media.clear();
    });
    panel
}

fn text_view(text: &str) -> gtk4::TextView {
    let view = gtk4::TextView::new();
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_wrap_mode(gtk4::WrapMode::WordChar);
    view.set_monospace(true);
    view.set_left_margin(12);
    view.set_right_margin(12);
    view.set_top_margin(12);
    view.set_bottom_margin(12);
    view.buffer().set_text(text);
    view
}

/// At most `PREVIEW_CHARS` of `text`, saying so when it is cut.
fn preview_text(text: &str) -> String {
    let mut limited: String = text.chars().take(PREVIEW_CHARS).collect();
    if limited.len() < text.len() {
        limited.push_str("\n\n[Preview truncated at 64K characters]");
    }
    limited
}

const PREVIEW_CHARS: usize = 64 * 1024;

/// Visible HTTP(S) references only, bounded and never fetched during discovery.
fn links(text: &str) -> Vec<String> {
    let mut offset = text.len().saturating_sub(512 * 1024);
    while !text.is_char_boundary(offset) { offset += 1; }
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for token in text[offset..].split(|c: char| c.is_whitespace() || c.is_control() || "<>\"'`".contains(c)) {
        let Some(candidate) = crate::terminal_links::clean_link(token) else { continue };
        if seen.insert(candidate.clone()) { result.push(candidate); }
        if result.len() == 100 { break; }
    }
    result
}

type Collapsed = Rc<RefCell<HashSet<String>>>;

fn group(content: &gtk4::Box, title: &str, count: usize, collapsed: &Collapsed) -> gtk4::Box {
    let panel = gtk4::Expander::new(None);
    let heading = gtk4::Label::new(Some(&format!("{title} · {count}")));
    heading.add_css_class("asset-group-title");
    heading.set_xalign(0.0);
    panel.set_label_widget(Some(&heading));
    panel.set_expanded(!collapsed.borrow().contains(title));
    panel.add_css_class("asset-group");
    let rows = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    rows.set_margin_top(8);
    panel.set_child(Some(&rows));
    let collapsed = Rc::clone(collapsed);
    let title = title.to_string();
    panel.connect_expanded_notify(move |panel| {
        if panel.is_expanded() { collapsed.borrow_mut().remove(&title); }
        else { collapsed.borrow_mut().insert(title.clone()); }
    });
    content.append(&panel);
    rows
}

fn show_links(content: &gtk4::Box, urls: &[String], collapsed: &Collapsed, status: &gtk4::Label) {
    if urls.is_empty() { return; }
    let rows = group(content, "Links", urls.len(), collapsed);
    for url in urls {
        let row = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
        let label = gtk4::Label::new(Some(url));
        label.set_selectable(true);
        label.set_wrap(true);
        label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
        label.set_xalign(0.0);
        row.append(&label);
        let actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        let open = gtk4::Button::with_label("Open link");
        let target = url.clone();
        let feedback = status.downgrade();
        open.connect_clicked(move |_| {
            let target = target.clone();
            let feedback = feedback.clone();
            glib::MainContext::default().spawn_local(async move {
                if let Err(error) = gio::AppInfo::launch_default_for_uri_future(&target, None::<&gio::AppLaunchContext>).await {
                    if let Some(status) = feedback.upgrade() { status.set_text(&format!("Could not open link: {error}")); }
                }
            });
        });
        let copy = gtk4::Button::with_label("Copy URL");
        let target = url.clone();
        let feedback = status.downgrade();
        copy.connect_clicked(move |button| {
            button.display().clipboard().set_text(&target);
            if let Some(status) = feedback.upgrade() { status.set_text("URL copied"); }
        });
        actions.append(&open);
        actions.append(&copy);
        row.append(&actions);
        rows.append(&row);
    }
}

/// The size a Files panel opens at, and the smallest it may be dragged to.
const DEFAULT_SIZE: (i32, i32) = (760, 600);
const MIN_SIZE: (i32, i32) = (460, 400);

/// Where Files panels open: the desktop's overlay, how tall its top bar is,
/// and the dialog they must stay under.
struct Host {
    overlay: glib::WeakRef<gtk4::Overlay>,
    top: Rc<dyn Fn() -> i32>,
    ceiling: glib::WeakRef<gtk4::Widget>,
}

/// An open Files panel. It belongs to no card: it stays until its own ✕.
struct Open {
    session: String,
    panel: Rc<crate::floating_panel::MovablePanel>,
    generation: Rc<Cell<u64>>,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
    static OPEN: RefCell<Vec<Open>> = const { RefCell::new(Vec::new()) };
    /// Where and how big the user last left a Files panel.
    static LAST: Cell<(Option<(i32, i32)>, Option<(i32, i32)>)> = const { Cell::new((None, None)) };
}

/// Open Files panels in `overlay`, below its bar (`top` tall) and under
/// `ceiling`, a dialog that stays above them.
pub fn set_host(overlay: &gtk4::Overlay, top: Rc<dyn Fn() -> i32>, ceiling: &impl IsA<gtk4::Widget>) {
    HOST.with(|host| {
        host.replace(Some(Host {
            overlay: overlay.downgrade(),
            top,
            ceiling: ceiling.upcast_ref::<gtk4::Widget>().downgrade(),
        }))
    });
}

/// Unsaved Markdown edits in a panel, and whether the user was warned once
/// that leaving would lose them.
#[derive(Clone, Default)]
struct Edits {
    dirty: Rc<Cell<bool>>,
    warned: Rc<Cell<bool>>,
}

impl Edits {
    /// Whether the shown file may be left: yes with no unsaved edits, or on the
    /// second try after a warning.
    fn may_leave(&self, status: &gtk4::Label) -> bool {
        if !self.dirty.get() || self.warned.get() {
            self.dirty.set(false);
            return true;
        }
        self.warned.set(true);
        status.set_text("Unsaved changes. Save them, or press again to discard.");
        false
    }
}

pub fn button(session: String, title: String) -> gtk4::Button {
    let button = gtk4::Button::from_icon_name("folder-symbolic");
    button.update_property(&[gtk4::accessible::Property::Label("Files")]);
    button.add_css_class("term-btn");
    button.set_tooltip_text(Some(
        "Files and links referenced by this terminal",
    ));
    // Kept for the card's lifetime, across closing and reopening its panel.
    let collapsed: Collapsed = Rc::new(RefCell::new(HashSet::new()));
    button.connect_clicked(move |_| open(&session, &title, &collapsed));
    button
}

/// Show the Files panel of `session`, bringing it to the front when it is
/// already open.
fn open(session: &str, title: &str, collapsed: &Collapsed) {
    let Some((overlay, top, ceiling)) = HOST.with(|host| {
        let host = host.borrow();
        let host = host.as_ref()?;
        Some((host.overlay.upgrade()?, Rc::clone(&host.top), host.ceiling.upgrade()))
    }) else {
        return;
    };
    let existing = OPEN.with(|open| {
        open.borrow().iter().find(|o| o.session == session).map(|o| Rc::clone(&o.panel))
    });
    if let Some(panel) = existing {
        if panel.is_in(&overlay) {
            panel.raise(ceiling.as_ref());
            return;
        }
        // Left in an overlay that is gone.
        close(session);
    }
    let drawer = build_drawer(session, title, collapsed);
    let (mut saved_pos, saved_size) = LAST.get();
    // Another open panel would hide this one exactly.
    let others = OPEN.with(|open| open.borrow().len()) as i32;
    saved_pos = saved_pos.map(|(x, y)| (x + 32 * others, y + 32 * others));
    let panel = crate::floating_panel::MovablePanel::install(
        &overlay,
        &drawer.widget,
        crate::floating_panel::PanelLayout { default_size: DEFAULT_SIZE, min_size: MIN_SIZE, saved_pos, saved_size },
        top,
        Rc::new(|position, size| LAST.set((Some(position), Some(size)))),
    );
    panel.raise(ceiling.as_ref());
    // A press anywhere in a panel brings it above the other panels.
    let press = gtk4::GestureClick::new();
    press.set_button(0);
    press.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let weak = Rc::downgrade(&panel);
    let ceiling = ceiling.map(|c| c.downgrade());
    press.connect_pressed(move |_, _, _, _| {
        if let Some(panel) = weak.upgrade() {
            panel.raise(ceiling.as_ref().and_then(|c| c.upgrade()).as_ref());
        }
    });
    drawer.widget.add_controller(press);
    let target = session.to_string();
    let edits = drawer.edits.clone();
    let status = drawer.status.clone();
    drawer.close.connect_clicked(move |_| {
        if edits.may_leave(&status) {
            close(&target);
        }
    });
    OPEN.with(|open| {
        open.borrow_mut().push(Open {
            session: session.to_string(),
            panel,
            generation: Rc::clone(&drawer.generation),
        })
    });
    (drawer.reload)(None);
}

/// Close the Files panel of `session`; results still loading are dropped.
fn close(session: &str) {
    let closed = OPEN.with(|open| {
        let mut open = open.borrow_mut();
        let index = open.iter().position(|o| o.session == session)?;
        Some(open.remove(index))
    });
    if let Some(closed) = closed {
        closed.generation.set(closed.generation.get() + 1);
        closed.panel.remove();
    }
}

struct Drawer {
    widget: gtk4::Box,
    close: gtk4::Button,
    status: gtk4::Label,
    edits: Edits,
    generation: Rc<Cell<u64>>,
    /// Lists the references again, with an extra path the user added.
    reload: Rc<dyn Fn(Option<String>)>,
}

fn clear(container: &gtk4::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn build_drawer(session: &str, card_title: &str, collapsed: &Collapsed) -> Drawer {
    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    outer.add_css_class("mini-terminal");
    outer.add_css_class("harness-panel");
    outer.add_css_class("asset-drawer");
    outer.set_size_request(MIN_SIZE.0, MIN_SIZE.1);
    // The header is where the panel is dragged from, like a terminal card's.
    let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    header.add_css_class("term-header");
    let badge = gtk4::Label::new(Some("📁"));
    badge.add_css_class("launcher-head-badge");
    badge.set_valign(gtk4::Align::Center);
    header.append(&badge);
    let titles = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    titles.set_hexpand(true);
    titles.set_valign(gtk4::Align::Center);
    let title = gtk4::Label::new(Some("Files & links"));
    title.add_css_class("term-title");
    title.set_halign(gtk4::Align::Start);
    let subtitle = gtk4::Label::new(Some(card_title));
    subtitle.add_css_class("launcher-subtitle");
    subtitle.set_halign(gtk4::Align::Start);
    subtitle.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    titles.append(&title);
    titles.append(&subtitle);
    header.append(&titles);
    let close = gtk4::Button::with_label("✕");
    close.set_tooltip_text(Some("Close Files & links"));
    close.add_css_class("term-btn");
    close.set_valign(gtk4::Align::Center);
    header.append(&close);
    outer.append(&header);

    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    body.set_vexpand(true);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    outer.append(&body);
    let controls = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    let path = gtk4::Entry::new();
    path.set_placeholder_text(Some("Workspace-relative file path…"));
    path.set_hexpand(true);
    let add = gtk4::Button::with_label("Add");
    let refresh = gtk4::Button::with_label("Refresh");
    controls.append(&path);
    controls.append(&add);
    controls.append(&refresh);
    body.append(&controls);
    let status = gtk4::Label::new(Some(
        "Only referenced files inside this terminal’s workspace. No folder scanning.",
    ));
    status.set_wrap(true);
    status.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    status.set_xalign(0.0);
    body.append(&status);
    // A Markdown preview's Rendered / Edit switch and Save.
    let bar = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    bar.set_visible(false);
    body.append(&bar);
    let scroll = gtk4::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .min_content_height(120)
        .build();
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    scroll.set_child(Some(&content));
    body.append(&scroll);
    let generation = Rc::new(Cell::new(0u64));
    let edits = Edits::default();
    let render: Rc<dyn Fn(crate::assets::Asset, u32)> = {
        let content = content.clone();
        let bar = bar.clone();
        let status = status.clone();
        let session = session.to_string();
        let generation = Rc::clone(&generation);
        let edits = edits.clone();
        Rc::new(move |asset, page| {
            generation.set(generation.get() + 1);
            let ticket = generation.get();
            clear(&content);
            status.set_text("Loading preview…");
            let content = content.clone();
            let bar = bar.clone();
            let status = status.clone();
            let generation = Rc::clone(&generation);
            let session = session.clone();
            let edits = edits.clone();
            glib::MainContext::default().spawn_local(async move {
                let name = asset.name.clone();
                let id = asset.id.clone();
                let reader = session.clone();
                let result = gio::spawn_blocking(move || {
                    let _permit = crate::assets::Transfer::acquire().ok_or("Preview busy")?;
                    let (meta, bytes) = crate::assets::read_desktop(&reader, &id)?;
                    if meta.kind == "text" || meta.kind == "markdown" {
                        let text = String::from_utf8(bytes).map_err(|_| "Invalid UTF-8")?;
                        Ok::<_, String>(if meta.kind == "markdown" {
                            Preview::Markdown(text)
                        } else {
                            Preview::Text(preview_text(&text))
                        })
                    } else if meta.kind == "audio" {
                        Ok(Preview::Audio(bytes))
                    } else {
                        let bytes = if meta.kind == "pdf" {
                            crate::asset_pdf::page(bytes, page)?
                        } else {
                            bytes
                        };
                        frames(&bytes).map(Preview::Images)
                    }
                })
                .await;
                if generation.get() != ticket {
                    return;
                }
                match result {
                    Ok(Ok(preview)) => {
                        clear(&content);
                        clear(&bar);
                        bar.set_visible(false);
                        status.set_text(&format!(
                            "{name}{}",
                            if asset.kind == "pdf" {
                                format!(" · page {page}")
                            } else {
                                String::new()
                            }
                        ));
                        match preview {
                            Preview::Text(text) => content.append(&text_view(&text)),
                            Preview::Audio(bytes) => content.append(&audio_preview(bytes)),
                            Preview::Markdown(text) => {
                                let preview = MarkdownPreview { content: &content, bar: &bar, status: &status, edits: &edits };
                                preview.show(&session, asset.clone(), text, (Rc::clone(&generation), ticket));
                            }
                            Preview::Images(frames) => {
                                let picture = gtk4::Picture::new();
                                picture.set_can_shrink(true);
                                let textures: Vec<_> = frames
                                    .into_iter()
                                    .map(|f| {
                                        let texture = gdk::MemoryTexture::new(
                                            f.width,
                                            f.height,
                                            if f.alpha {
                                                gdk::MemoryFormat::R8g8b8a8
                                            } else {
                                                gdk::MemoryFormat::R8g8b8
                                            },
                                            &glib::Bytes::from_owned(f.pixels),
                                            f.stride,
                                        );
                                        (texture, f.delay)
                                    })
                                    .collect();
                                if let Some((texture, _)) = textures.first() {
                                    picture.set_paintable(Some(texture));
                                }
                                picture.set_size_request(400, 300);
                                picture.set_vexpand(true);
                                content.append(&picture);
                                let zoom = gtk4::Scale::with_range(
                                    gtk4::Orientation::Horizontal,
                                    1.0,
                                    4.0,
                                    0.25,
                                );
                                zoom.set_value(1.0);
                                let weak_picture = picture.downgrade();
                                zoom.connect_value_changed(move |scale| {
                                    if let Some(picture) = weak_picture.upgrade() {
                                        picture.set_size_request(
                                            (400.0 * scale.value()) as i32,
                                            (300.0 * scale.value()) as i32,
                                        );
                                    }
                                });
                                content.append(&zoom);
                                if textures.len() > 1 {
                                    let weak_picture = picture.downgrade();
                                    let mut index = 0usize;
                                    let mut next = std::time::Instant::now() + textures[0].1;
                                    glib::timeout_add_local(Duration::from_millis(30), move || {
                                        if generation.get() != ticket {
                                            return glib::ControlFlow::Break;
                                        }
                                        let Some(picture) = weak_picture.upgrade() else {
                                            return glib::ControlFlow::Break;
                                        };
                                        if std::time::Instant::now() >= next {
                                            index = (index + 1) % textures.len();
                                            picture.set_paintable(Some(&textures[index].0));
                                            next = std::time::Instant::now() + textures[index].1;
                                        }
                                        glib::ControlFlow::Continue
                                    });
                                }
                            }
                        }
                    }
                    Ok(Err(error)) => status.set_text(&error.replace('_', " ")),
                    Err(_) => status.set_text("Preview worker failed"),
                }
            });
        })
    };
    let selected = Rc::new(RefCell::new(None::<crate::assets::Asset>));
    let page = Rc::new(Cell::new(1u32));
    let pages = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    for (label, delta) in [("Previous page", -1i32), ("Next page", 1)] {
        let button = gtk4::Button::with_label(label);
        let selected = Rc::clone(&selected);
        let page = Rc::clone(&page);
        let render = Rc::clone(&render);
        button.connect_clicked(move |_| {
            if let Some(asset) = selected.borrow().as_ref().filter(|a| a.kind == "pdf") {
                page.set((page.get() as i32 + delta).clamp(1, 10000) as u32);
                render(asset.clone(), page.get());
            }
        });
        pages.append(&button);
    }
    pages.set_visible(false);
    body.append(&pages);
    let reload: Rc<dyn Fn(Option<String>)> = {
        let session = session.to_string();
        let generation = Rc::clone(&generation);
        let content = content.clone();
        let bar = bar.clone();
        let status = status.clone();
        let collapsed = Rc::clone(collapsed);
        let edits = edits.clone();
        Rc::new(move |explicit| {
            generation.set(generation.get() + 1);
            let ticket = generation.get();
            pages.set_visible(false);
            clear(&bar);
            bar.set_visible(false);
            edits.dirty.set(false);
            selected.borrow_mut().take();
            clear(&content);
            status.set_text("Finding referenced files and links…");
            let session = session.clone();
            let generation = Rc::clone(&generation);
            let content = content.clone();
            let status = status.clone();
            let render = Rc::clone(&render);
            let selected = Rc::clone(&selected);
            let page = Rc::clone(&page);
            let pages = pages.clone();
            let collapsed = Rc::clone(&collapsed);
            glib::MainContext::default().spawn_local(async move {
                let result = gio::spawn_blocking(move || {
                    let urls = links(&crate::tmux::capture_pane_history(&session).unwrap_or_default());
                    (urls, crate::assets::list_desktop(&session, explicit.as_deref()))
                }).await;
                if generation.get() != ticket { return; }
                clear(&content);
                match result {
                    Ok((urls, files)) => {
                        show_links(&content, &urls, &collapsed, &status);
                        match files {
                            Ok(items) => {
                                status.set_text(&format!("{} files · {} links. Choose a file to preview; Refresh returns to this list.", items.len(), urls.len()));
                                for (kind, title) in [("markdown", "Markdown"), ("image", "Images"), ("audio", "Audio"), ("pdf", "PDFs"), ("text", "Text / code")] {
                                    let files: Vec<_> = items.iter().filter(|item| item.kind == kind).collect();
                                    if files.is_empty() { continue; }
                                    let rows = group(&content, title, files.len(), &collapsed);
                                    for item in files {
                                        let item = item.clone();
                                        let button = gtk4::Button::new();
                                        let label = gtk4::Label::new(Some(&format!("{}  ·  {} KB", item.relative_path, item.size.div_ceil(1024))));
                                        label.set_wrap(true);
                                        label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
                                        label.set_xalign(0.0);
                                        button.set_child(Some(&label));
                                        let render = Rc::clone(&render); let selected = Rc::clone(&selected); let page = Rc::clone(&page); let pages = pages.clone();
                                        button.connect_clicked(move |_| { page.set(1); pages.set_visible(item.kind == "pdf"); *selected.borrow_mut() = Some(item.clone()); render(item.clone(), 1); });
                                        rows.append(&button);
                                    }
                                }
                                if items.is_empty() {
                                    let empty = gtk4::Label::new(Some("No supported files found. Add a workspace-relative path above."));
                                    empty.set_wrap(true);
                                    content.append(&empty);
                                }
                            }
                            Err(error) => status.set_text(&format!("{} links · Files: {}", urls.len(), error.replace('_', " "))),
                        }
                    }
                    Err(_) => status.set_text("Reference lookup failed"),
                }
            });
        })
    };
    refresh.connect_clicked({
        let reload = Rc::clone(&reload);
        let edits = edits.clone();
        let status = status.clone();
        move |_| {
            if edits.may_leave(&status) {
                reload(None)
            }
        }
    });
    add.connect_clicked({
        let reload = Rc::clone(&reload);
        let edits = edits.clone();
        let status = status.clone();
        move |_| {
            if edits.may_leave(&status) {
                reload(Some(path.text().to_string()))
            }
        }
    });
    Drawer { widget: outer, close, status, edits, generation, reload }
}

/// Where a Markdown file is shown: its content area, the bar above it for
/// the mode switch and Save, the status line, and the panel's unsaved edits.
struct MarkdownPreview<'a> {
    content: &'a gtk4::Box,
    bar: &'a gtk4::Box,
    status: &'a gtk4::Label,
    edits: &'a Edits,
}

impl MarkdownPreview<'_> {
    /// Show `source` rendered, with Edit switching to the text itself, which
    /// Save writes back to the file. `shown` is the panel's preview counter
    /// and this preview's ticket in it: a save that ends after the user moved
    /// on leaves the next preview's state alone.
    fn show(&self, session: &str, asset: crate::assets::Asset, source: String, shown: (Rc<Cell<u64>>, u64)) {
        let content = self.content.clone();
        let status = self.status.clone();
        let edits = self.edits.clone();
        let rendered = crate::markdown_view::view(&preview_text(&source));
        let editor = gtk4::TextView::new();
        editor.add_css_class("markdown-editor");
        editor.set_monospace(true);
        editor.set_wrap_mode(gtk4::WrapMode::WordChar);
        editor.set_left_margin(12);
        editor.set_right_margin(12);
        editor.set_top_margin(12);
        editor.set_bottom_margin(12);
        editor.set_vexpand(true);
        editor.buffer().set_text(&source);
        let show_rendered = gtk4::ToggleButton::with_label("Rendered");
        show_rendered.set_tooltip_text(Some("Show the formatted document"));
        let show_edit = gtk4::ToggleButton::with_label("Edit");
        show_edit.set_tooltip_text(Some("Edit the Markdown text"));
        show_edit.set_group(Some(&show_rendered));
        show_rendered.set_active(true);
        let save = gtk4::Button::with_label("Save");
        save.set_tooltip_text(Some("Save the edited file (Ctrl+S)"));
        save.set_sensitive(false);
        save.set_visible(false);
        self.bar.append(&show_rendered);
        self.bar.append(&show_edit);
        self.bar.append(&save);
        self.bar.set_visible(true);
        content.append(&rendered);
        edits.dirty.set(false);
        edits.warned.set(false);

        let name = asset.name.clone();
        editor.buffer().connect_changed({
            let edits = edits.clone();
            let save = save.clone();
            let status = status.clone();
            let name = name.clone();
            move |_| {
                edits.dirty.set(true);
                edits.warned.set(false);
                save.set_sensitive(true);
                save.set_visible(true);
                status.set_text(&format!("{name} · unsaved changes"));
            }
        });
        show_edit.connect_toggled({
            let content = content.clone();
            let rendered = rendered.clone();
            let editor = editor.clone();
            let save = save.clone();
            let edits = edits.clone();
            move |button| {
                clear(&content);
                if button.is_active() {
                    content.append(&editor);
                    save.set_visible(true);
                    editor.grab_focus();
                } else {
                    // Show the edits, saved or not.
                    let buffer = editor.buffer();
                    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
                    crate::markdown_view::render(&rendered, &preview_text(&text));
                    content.append(&rendered);
                    save.set_visible(edits.dirty.get());
                }
            }
        });
        let id = Rc::new(RefCell::new(asset.id));
        let session = session.to_string();
        save.connect_clicked({
            let buffer = editor.buffer();
            move |save| {
                let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
                save.set_sensitive(false);
                status.set_text(&format!("Saving {name}…"));
                let (session, current) = (session.clone(), id.borrow().clone());
                let (id, buffer, save, status, edits, name) =
                    (Rc::clone(&id), buffer.clone(), save.clone(), status.clone(), edits.clone(), name.clone());
                let (generation, ticket) = (Rc::clone(&shown.0), shown.1);
                glib::MainContext::default().spawn_local(async move {
                    let written = text.clone();
                    let result =
                        gio::spawn_blocking(move || crate::assets::write_markdown(&session, &current, &written)).await;
                    if generation.get() != ticket {
                        return;
                    }
                    match result {
                        Ok(Ok(saved)) => {
                            *id.borrow_mut() = saved.id;
                            let now = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
                            if now == text.as_str() {
                                edits.dirty.set(false);
                                status.set_text(&format!("Saved {name}"));
                            } else {
                                save.set_sensitive(true);
                                status.set_text(&format!("Saved {name}; newer edits are not saved yet"));
                            }
                        }
                        Ok(Err(error)) => {
                            save.set_sensitive(true);
                            status.set_text(&format!("Not saved: {}", error.replace('_', " ")));
                        }
                        Err(_) => {
                            save.set_sensitive(true);
                            status.set_text("Not saved: the save worker failed");
                        }
                    }
                });
            }
        });
        let keys = gtk4::EventControllerKey::new();
        keys.connect_key_pressed({
            let save = save.downgrade();
            move |_, key, _, modifiers| {
                let Some(save) = save.upgrade() else { return glib::Propagation::Proceed };
                if modifiers.contains(gdk::ModifierType::CONTROL_MASK) && matches!(key, gdk::Key::s | gdk::Key::S) {
                    if save.is_sensitive() {
                        save.emit_clicked();
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        });
        editor.add_controller(keys);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn links_preserve_targets_and_strip_markdown_wrappers() {
        assert_eq!(links("[Docs](https://example.org/a?q=1&b=2#part). <http://localhost:3000/> https://example.org/a?q=1&b=2#part"),
            vec!["https://example.org/a?q=1&b=2#part", "http://localhost:3000/"]);
        assert_eq!(links("(https://example.org/Thing_(test)) https://example.org/資料"),
            vec!["https://example.org/Thing_(test)", "https://example.org/資料"]);
        assert!(links("file:///tmp/a javascript:alert(1) https://user:password@example.org").is_empty());
        assert!(links(&format!("https://example.org/{}", "a".repeat(9000))).is_empty());
        assert_eq!(links(&(0..200).map(|n| format!("https://example.org/{n} ")).collect::<String>()).len(), 100);
        assert!(links(&format!("https://example.org/ {}", "界".repeat(200_000))).is_empty());
    }

    #[test]
    fn malformed_gifs_are_bounded() {
        assert!(crate::assets::gif_frames(b"GIF89a").is_err());
        assert!(frames(b"not an image").is_err());
    }

    #[test]
    fn decodes_gif_frames_off_the_gtk_thread() {
        let mut gif = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff".to_vec();
        let frame = b"\x21\xf9\x04\x00\x0a\x00\x00\x00\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00";
        gif.extend_from_slice(frame);
        gif.extend_from_slice(frame);
        gif.push(0x3b);
        assert_eq!(crate::assets::gif_frames(&gif).unwrap(), 2);
        let frames = std::thread::spawn(move || frames(&gif))
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].width, 1);
    }

    #[test]
    fn audio_preview_is_explicit_and_releases_media_with_its_panel() {
        if !crate::gtk_test::is_child() {
            crate::gtk_test::run_in_child_process(
                "asset_view::tests::audio_preview_is_explicit_and_releases_media_with_its_panel",
            );
            return;
        }
        gtk4::init().unwrap();
        let panel = audio_preview(b"RIFF\0\0\0\0WAVE".to_vec());
        let controls = panel.last_child().unwrap().downcast::<gtk4::MediaControls>().unwrap();
        let media = controls.media_stream().unwrap();
        assert!(!media.is_playing());
        let file = media.clone().downcast::<gtk4::MediaFile>().unwrap();
        let window = gtk4::Window::new();
        window.set_child(Some(&panel));
        window.present();
        crate::gtk_test::pump(50);
        assert!(file.input_stream().is_some());
        window.set_child(gtk4::Widget::NONE);
        assert!(!media.is_playing());
        assert!(file.input_stream().is_none());
        window.set_child(Some(&panel));
        crate::gtk_test::pump(50);
        assert!(file.input_stream().is_some());
        assert!(!media.is_playing());
        window.set_child(gtk4::Widget::NONE);
        window.close();
        drop(controls);
        drop(media);
        drop(panel);
        crate::gtk_test::pump(50);
        assert!(file.input_stream().is_none());
    }

    #[test]
    fn drawer_is_lazy_and_uses_no_terminal_input() {
        if !crate::gtk_test::is_child() {
            crate::gtk_test::run_in_child_process(
                "asset_view::tests::drawer_is_lazy_and_uses_no_terminal_input",
            );
            return;
        }
        if gtk4::init().is_err() {
            return;
        }
        let button = button("test_missing_asset_terminal".into(), "Claude".into());
        assert_eq!(button.icon_name().as_deref(), Some("folder-symbolic"));
        assert!(button.label().is_none());
        // Nothing is listed until the panel is opened.
        let collapsed: Collapsed = Rc::new(RefCell::new(HashSet::new()));
        let drawer = build_drawer("test_missing_asset_terminal", "Claude", &collapsed);
        assert!(drawer.widget.first_child().unwrap().has_css_class("term-header"));
        assert!(!drawer.widget.is_mapped());
        assert_eq!(drawer.generation.get(), 0);
        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
        let collapsed: Collapsed = Rc::new(RefCell::new(HashSet::new()));
        for title in ["Links", "Markdown", "Images", "Audio", "PDFs", "Text / code"] {
            let rows = group(&content, title, 2, &collapsed);
            rows.append(&gtk4::Label::new(Some("Reference")));
            let panel = content.last_child().unwrap().downcast::<gtk4::Expander>().unwrap();
            assert!(panel.is_expanded());
            panel.set_expanded(false);
            assert!(collapsed.borrow().contains(title));
            content.remove(&panel);
            group(&content, title, 3, &collapsed);
            let panel = content.last_child().unwrap().downcast::<gtk4::Expander>().unwrap();
            assert!(!panel.is_expanded());
            panel.set_expanded(true);
            assert!(!collapsed.borrow().contains(title));
        }
        let status = gtk4::Label::new(None);
        let links_content = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
        show_links(&links_content, &["https://example.org/".into()], &collapsed, &status);
        assert!(links_content.first_child().unwrap().is::<gtk4::Expander>());

        let text = text_view("# Title\n<script>ignored</script>");
        let buffer = text.buffer();
        let literal = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
        assert_eq!(literal, "# Title\n<script>ignored</script>");
    }

    #[test]
    fn files_panel_floats_on_its_own_and_markdown_switches_modes() {
        if !crate::gtk_test::is_child() {
            crate::gtk_test::run_in_child_process(
                "asset_view::tests::files_panel_floats_on_its_own_and_markdown_switches_modes",
            );
            return;
        }
        if gtk4::init().is_err() {
            return;
        }
        crate::styles::apply_styles();
        let overlay = gtk4::Overlay::new();
        overlay.set_child(Some(&gtk4::Box::new(gtk4::Orientation::Vertical, 0)));
        let ceiling = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        overlay.add_overlay(&ceiling);
        set_host(&overlay, Rc::new(|| 46), &ceiling);
        let collapsed: Collapsed = Rc::new(RefCell::new(HashSet::new()));
        let open_count = || OPEN.with(|open| open.borrow().len());
        // Opened as its own panel in the overlay, under the dialog that stays on top.
        open("test_files_a", "Claude", &collapsed);
        assert_eq!(open_count(), 1);
        let shell = ceiling.prev_sibling().unwrap();
        assert!(shell.first_child().unwrap().has_css_class("asset-drawer"));
        // Its button again brings the same panel forward rather than a second one.
        open("test_files_b", "Codex", &collapsed);
        assert_eq!(open_count(), 2);
        assert_ne!(ceiling.prev_sibling().unwrap(), shell);
        open("test_files_a", "Claude", &collapsed);
        assert_eq!(open_count(), 2);
        assert_eq!(ceiling.prev_sibling().unwrap(), shell);
        // Each closes by itself and leaves the overlay.
        close("test_files_a");
        assert_eq!(open_count(), 1);
        assert!(shell.parent().is_none());
        close("test_files_b");
        assert_eq!(open_count(), 0);
        assert_eq!(ceiling.prev_sibling().unwrap(), overlay.first_child().unwrap());

        // Markdown opens rendered; Edit shows the text itself.
        let (content, bar) = (gtk4::Box::new(gtk4::Orientation::Vertical, 0), gtk4::Box::new(gtk4::Orientation::Horizontal, 0));
        let status = gtk4::Label::new(None);
        let edits = Edits::default();
        let asset = crate::assets::Asset {
            id: "id".into(),
            name: "plan.md".into(),
            relative_path: "plan.md".into(),
            mime_type: "text/markdown".into(),
            kind: "markdown".into(),
            size: 9,
            modified: 0,
        };
        MarkdownPreview { content: &content, bar: &bar, status: &status, edits: &edits }
            .show("test_files_a", asset, "# **Plan**".into(), (Rc::new(Cell::new(1)), 1));
        let shown = |content: &gtk4::Box| {
            let view = content.first_child().unwrap().downcast::<gtk4::TextView>().unwrap();
            let buffer = view.buffer();
            (view.is_editable(), buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string())
        };
        assert_eq!(shown(&content), (false, "Plan".into()));
        let rendered_mode = bar.first_child().unwrap().downcast::<gtk4::ToggleButton>().unwrap();
        let edit_mode = rendered_mode.next_sibling().unwrap().downcast::<gtk4::ToggleButton>().unwrap();
        let save = edit_mode.next_sibling().unwrap().downcast::<gtk4::Button>().unwrap();
        assert!(rendered_mode.is_active() && !save.is_visible());
        edit_mode.set_active(true);
        assert_eq!(shown(&content), (true, "# **Plan**".into()));
        assert!(save.is_visible() && !save.is_sensitive());
        let editor = content.first_child().unwrap().downcast::<gtk4::TextView>().unwrap();
        editor.buffer().insert(&mut editor.buffer().end_iter(), "\n\n- step");
        assert!(edits.dirty.get() && save.is_sensitive());
        // Back to rendered shows the unsaved edits, and Save stays offered.
        rendered_mode.set_active(true);
        assert_eq!(shown(&content), (false, "Plan\n\n• step".into()));
        assert!(save.is_visible());
        // Leaving with unsaved edits warns once, then lets go.
        assert!(!edits.may_leave(&status));
        assert!(status.text().contains("Unsaved"));
        assert!(edits.may_leave(&status));
    }
}
