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
    Text(String, bool),
    Images(Vec<Frame>),
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

fn text_view(text: &str, markdown: bool) -> gtk4::TextView {
    let view = gtk4::TextView::new();
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_wrap_mode(gtk4::WrapMode::WordChar);
    view.set_monospace(!markdown);
    view.set_left_margin(12);
    view.set_right_margin(12);
    view.set_top_margin(12);
    view.set_bottom_margin(12);
    let buffer = view.buffer();
    let heading = gtk4::TextTag::builder()
        .name("heading")
        .weight(700)
        .scale(1.3)
        .build();
    let code = gtk4::TextTag::builder()
        .name("code")
        .family("monospace")
        .build();
    buffer.tag_table().add(&heading);
    buffer.tag_table().add(&code);
    let mut fenced = false;
    for line in text.lines() {
        if markdown && line.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        let mut end = buffer.end_iter();
        if markdown && !fenced && line.starts_with('#') && line.contains(' ') {
            buffer.insert_with_tags(
                &mut end,
                &format!("{}\n", line.trim_start_matches('#').trim_start()),
                &[&heading],
            );
        } else if fenced {
            buffer.insert_with_tags(&mut end, &format!("{line}\n"), &[&code]);
        } else {
            buffer.insert(&mut end, &format!("{line}\n"));
        }
    }
    view
}

/// Visible HTTP(S) references only, bounded and never fetched during discovery.
fn links(text: &str) -> Vec<String> {
    let mut offset = text.len().saturating_sub(512 * 1024);
    while !text.is_char_boundary(offset) { offset += 1; }
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for token in text[offset..].split(|c: char| c.is_whitespace() || c.is_control() || "<>\"'`".contains(c)) {
        let lower = token.to_ascii_lowercase();
        let Some(start) = [lower.find("https://"), lower.find("http://")].into_iter().flatten().min() else { continue };
        let mut candidate = &token[start..];
        if candidate.len() > 8192 { continue; }
        loop {
            let old = candidate;
            candidate = candidate.trim_end_matches(['.', ',', ';', ':', '!', '?']);
            for (close, open) in [(')', '('), (']', '['), ('}', '{')] {
                if candidate.ends_with(close) && candidate.matches(close).count() > candidate.matches(open).count() {
                    candidate = &candidate[..candidate.len() - 1];
                }
            }
            if old == candidate { break; }
        }
        let Ok(url) = reqwest::Url::parse(candidate) else { continue };
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none()
            || !url.username().is_empty() || url.password().is_some() { continue; }
        if seen.insert(candidate.to_string()) { result.push(candidate.to_string()); }
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

pub fn button(session: String) -> gtk4::Button {
    let button = gtk4::Button::from_icon_name("folder-symbolic");
    button.update_property(&[gtk4::accessible::Property::Label("Files")]);
    button.add_css_class("term-btn");
    button.set_tooltip_text(Some(
        "Files and links referenced by this terminal",
    ));
    let slot: Rc<RefCell<Option<gtk4::Popover>>> = Rc::new(RefCell::new(None));
    button.connect_unmap({
        let slot = Rc::clone(&slot);
        move |_| {
            if let Some(pop) = slot.borrow().as_ref() {
                pop.popdown();
            }
        }
    });
    button.connect_unrealize({
        let slot = Rc::clone(&slot);
        move |_| {
            if let Some(pop) = slot.borrow_mut().take() {
                pop.popdown();
                pop.unparent();
            }
        }
    });
    button.connect_clicked(move |button| {
        if let Some(pop) = slot.borrow().as_ref() {
            pop.popup();
            return;
        }
        let pop = build_drawer(&session);
        pop.set_parent(button);
        pop.popup();
        *slot.borrow_mut() = Some(pop);
    });
    button
}

fn build_drawer(session: &str) -> gtk4::Popover {
    let pop = gtk4::Popover::new();
    pop.set_has_arrow(false);
    pop.set_autohide(false);
    pop.add_css_class("asset-drawer");
    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    body.set_size_request(620, 440);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let title = gtk4::Label::new(Some("Files & links"));
    title.set_hexpand(true);
    title.set_xalign(0.0);
    let close = gtk4::Button::with_label("Close");
    let weak_pop = pop.downgrade();
    close.connect_clicked(move |_| {
        if let Some(pop) = weak_pop.upgrade() {
            pop.popdown();
        }
    });
    header.append(&title);
    header.append(&close);
    body.append(&header);
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
    status.set_xalign(0.0);
    body.append(&status);
    let scroll = gtk4::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .min_content_height(300)
        .build();
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    scroll.set_child(Some(&content));
    body.append(&scroll);
    pop.set_child(Some(&body));
    let generation = Rc::new(Cell::new(0u64));
    pop.connect_closed({
        let generation = Rc::clone(&generation);
        move |_| generation.set(generation.get() + 1)
    });
    let render: Rc<dyn Fn(crate::assets::Asset, u32)> = {
        let content = content.clone();
        let status = status.clone();
        let session = session.to_string();
        let generation = Rc::clone(&generation);
        Rc::new(move |asset, page| {
            generation.set(generation.get() + 1);
            let ticket = generation.get();
            status.set_text("Loading preview…");
            let content = content.clone();
            let status = status.clone();
            let generation = Rc::clone(&generation);
            let session = session.clone();
            glib::MainContext::default().spawn_local(async move {
                let name = asset.name.clone();
                let id = asset.id.clone();
                let result = gio::spawn_blocking(move || {
                    let _permit = crate::assets::Transfer::acquire().ok_or("Preview busy")?;
                    let (meta, bytes) = crate::assets::read(&session, &id)?;
                    if meta.kind == "text" || meta.kind == "markdown" {
                        let text = String::from_utf8(bytes).map_err(|_| "Invalid UTF-8")?;
                        let mut limited: String = text.chars().take(64 * 1024).collect();
                        if limited.len() < text.len() {
                            limited.push_str("\n\n[Preview truncated at 64K characters]");
                        }
                        Ok::<_, String>(Preview::Text(limited, meta.kind == "markdown"))
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
                        while let Some(child) = content.first_child() {
                            content.remove(&child);
                        }
                        status.set_text(&format!(
                            "{name}{}",
                            if asset.kind == "pdf" {
                                format!(" · page {page}")
                            } else {
                                String::new()
                            }
                        ));
                        match preview {
                            Preview::Text(text, markdown) => {
                                content.append(&text_view(&text, markdown))
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
                                picture.set_size_request(560, 340);
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
                                            (560.0 * scale.value()) as i32,
                                            (340.0 * scale.value()) as i32,
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
    let collapsed: Collapsed = Rc::new(RefCell::new(HashSet::new()));
    let reload: Rc<dyn Fn(Option<String>)> = {
        let session = session.to_string();
        let generation = Rc::clone(&generation);
        let content = content.clone();
        let status = status.clone();
        Rc::new(move |explicit| {
            generation.set(generation.get() + 1);
            let ticket = generation.get();
            pages.set_visible(false);
            selected.borrow_mut().take();
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
                    (urls, crate::assets::list(&session, explicit.as_deref()))
                }).await;
                if generation.get() != ticket { return; }
                while let Some(child) = content.first_child() { content.remove(&child); }
                match result {
                    Ok((urls, files)) => {
                        show_links(&content, &urls, &collapsed, &status);
                        match files {
                            Ok(items) => {
                                status.set_text(&format!("{} files · {} links. Choose a file to preview; Refresh returns to this list.", items.len(), urls.len()));
                                for (kind, title) in [("markdown", "Markdown"), ("image", "Images"), ("pdf", "PDFs"), ("text", "Text / code")] {
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
        move |_| reload(None)
    });
    add.connect_clicked({
        let reload = Rc::clone(&reload);
        move |_| reload(Some(path.text().to_string()))
    });
    pop.connect_map(move |_| reload(None));
    pop
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
        let button = button("test_missing_asset_terminal".into());
        assert_eq!(button.icon_name().as_deref(), Some("folder-symbolic"));
        assert!(button.label().is_none());
        let pop = build_drawer("test_missing_asset_terminal");
        assert!(pop.child().is_some());
        assert!(!pop.is_mapped());
        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
        let collapsed: Collapsed = Rc::new(RefCell::new(HashSet::new()));
        for title in ["Links", "Markdown", "Images", "PDFs", "Text / code"] {
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

        let text = text_view(
            "# Title\n<script>ignored</script>\n![image](https://example/image.png)",
            true,
        );
        let buffer = text.buffer();
        let literal = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
        assert!(literal.contains("<script>ignored</script>"));
        assert!(literal.contains("https://example/image.png"));
    }
}
