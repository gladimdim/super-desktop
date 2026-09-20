//! Lazy file drawer. All filesystem work and decoding stays off GTK's thread.
use gtk4::gdk_pixbuf;
use gtk4::{gdk, gio, glib, prelude::*};
use std::cell::{Cell, RefCell};
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

pub fn button(session: String) -> gtk4::Button {
    let button = gtk4::Button::with_label("Files");
    button.add_css_class("term-btn");
    button.set_tooltip_text(Some(
        "Referenced files: images, GIF, PDF, Markdown and text",
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
    let title = gtk4::Label::new(Some("Referenced files"));
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
            status.set_text("Finding referenced files…");
            let session = session.clone();
            let generation = Rc::clone(&generation);
            let content = content.clone();
            let status = status.clone();
            let render = Rc::clone(&render);
            let selected = Rc::clone(&selected);
            let page = Rc::clone(&page);
            let pages = pages.clone();
            glib::MainContext::default().spawn_local(async move {
                let result = gio::spawn_blocking(move || crate::assets::list(&session, explicit.as_deref())).await;
                if generation.get() != ticket { return; }
                while let Some(child) = content.first_child() { content.remove(&child); }
                match result {
                    Ok(Ok(items)) => {
                        status.set_text(if items.is_empty() { "No files found in the current terminal output. Add a relative path above." } else { "Choose a file. Refresh returns to this list." });
                        for item in items {
                            let button = gtk4::Button::with_label(&format!("{}  ·  {}  ·  {} KB", item.relative_path, item.kind, item.size.div_ceil(1024)));
                            let render = Rc::clone(&render); let selected = Rc::clone(&selected); let page = Rc::clone(&page); let pages = pages.clone();
                            button.connect_clicked(move |_| { page.set(1); pages.set_visible(item.kind == "pdf"); *selected.borrow_mut() = Some(item.clone()); render(item.clone(), 1); });
                            content.append(&button);
                        }
                    }
                    Ok(Err(error)) => status.set_text(&error.replace('_', " ")),
                    Err(_) => status.set_text("File lookup failed"),
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
        assert_eq!(button.label().as_deref(), Some("Files"));
        let pop = build_drawer("test_missing_asset_terminal");
        assert!(pop.child().is_some());
        assert!(!pop.is_mapped());
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
