//! SD Editor's previews of images, sound and video, in a tab like a file.
//! Images use the Files panel's decoder (zoom, animated GIFs), decoded off
//! GTK's thread; sound and video play from the file with GTK's media player
//! (GStreamer). Playback pauses when its tab is hidden or the overlay hides,
//! and stops when the tab closes. A format this PC cannot play says which
//! packages add it.
use gtk4::prelude::*;
use gtk4::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Larger images are not read.
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
/// What adds the formats GStreamer's base plugins cannot play (WAV, MP3,
/// MP4, WebM, H.264…).
pub const MORE_FORMATS: &str = "sudo pacman -S --needed gst-plugins-good gst-libav";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Image,
    Audio,
    Video,
}

/// What a file is, from its name (its MIME type on Linux).
pub fn kind(path: &Path) -> Kind {
    let (content_type, _) = gio::content_type_guess(Some(path), None);
    let mime = content_type.as_str();
    if mime.starts_with("image/") {
        Kind::Image
    } else if mime.starts_with("audio/") || mime == "application/ogg" {
        Kind::Audio
    } else if mime.starts_with("video/") {
        Kind::Video
    } else {
        Kind::Text
    }
}

/// "1.2 MB" for a size in bytes.
fn size_text(bytes: u64) -> String {
    match bytes {
        b if b >= 1_000_000_000 => format!("{:.1} GB", b as f64 / 1e9),
        b if b >= 1_000_000 => format!("{:.1} MB", b as f64 / 1e6),
        b if b >= 1_000 => format!("{:.0} KB", b as f64 / 1e3),
        b => format!("{b} B"),
    }
}

/// GStreamer's error without its element path and debug lines:
/// "Error from element /GstPlayBin3:…/typefind: Could not determine type of
/// stream.\n../gst/…" is "Could not determine type of stream."
fn plain_error(message: &str) -> String {
    let first = message.lines().next().unwrap_or("").trim();
    let text = match first.strip_prefix("Error from element") {
        Some(rest) => rest.rsplit(": ").next().unwrap_or(rest).trim(),
        None => first,
    };
    let text = if text.is_empty() { "unknown error" } else { text };
    if text.ends_with('.') { text.to_owned() } else { format!("{text}.") }
}

/// "1:05" for a length in microseconds.
fn length_text(micros: i64) -> String {
    let seconds = micros / 1_000_000;
    match seconds {
        s if s >= 3600 => format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60),
        s => format!("{}:{:02}", s / 60, s % 60),
    }
}

pub struct Preview {
    pub kind: Kind,
    pub path: RefCell<PathBuf>,
    /// The notebook page.
    pub page: gtk4::Box,
    pub tab: gtk4::Box,
    pub close: gtk4::Button,
    label: gtk4::Label,
    media: Option<gtk4::MediaFile>,
    /// Shown on the status line while its tab is open: size, pixels, length.
    pub info: RefCell<String>,
    /// Told when `info` changes (a length is known once the file is open).
    pub on_info: RefCell<Option<Rc<dyn Fn()>>>,
    /// An animated image plays while this is 0.
    generation: Rc<Cell<u64>>,
}

impl Preview {
    pub fn new(path: &Path, kind: Kind) -> Rc<Self> {
        let page = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        page.add_css_class("sd-editor-preview");
        page.set_vexpand(true);
        page.set_hexpand(true);
        let tab = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        let label = gtk4::Label::new(None);
        label.add_css_class("sd-editor-tab-label");
        let close = gtk4::Button::with_label("✕");
        close.add_css_class("term-btn");
        close.add_css_class("sd-editor-tab-close");
        close.set_tooltip_text(Some("Close (Ctrl+W)"));
        close.set_focus_on_click(false);
        tab.append(&label);
        tab.append(&close);
        let media = matches!(kind, Kind::Audio | Kind::Video).then(|| gtk4::MediaFile::for_filename(path));
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let preview = Rc::new(Self {
            kind,
            path: RefCell::new(path.to_path_buf()),
            page,
            tab,
            close,
            label,
            media,
            info: RefCell::new(size_text(size)),
            on_info: RefCell::new(None),
            generation: Rc::new(Cell::new(0)),
        });
        preview.refresh_title();
        match kind {
            Kind::Image => preview.build_image(size),
            Kind::Audio | Kind::Video => preview.build_player(size),
            Kind::Text => {}
        }
        preview
    }

    pub fn title(&self) -> String {
        let path = self.path.borrow();
        path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
    }

    fn refresh_title(&self) {
        let icon = match self.kind {
            Kind::Image => "🖼",
            Kind::Audio => "♪",
            Kind::Video => "▶",
            Kind::Text => "",
        };
        self.label.set_text(&format!("{icon} {}", self.title()));
        self.tab.set_tooltip_text(Some(&self.path.borrow().display().to_string()));
    }

    fn set_info(&self, info: String) {
        *self.info.borrow_mut() = info;
        if let Some(on_info) = self.on_info.borrow().clone() {
            on_info();
        }
    }

    fn message(text: &str) -> gtk4::Label {
        let label = gtk4::Label::new(Some(text));
        label.add_css_class("sd-editor-empty");
        label.set_wrap(true);
        label.set_selectable(true);
        label
    }

    /// Reads and decodes the image off GTK's thread, then shows it with a
    /// zoom slider (an animated GIF plays).
    fn build_image(self: &Rc<Self>, size: u64) {
        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        content.set_vexpand(true);
        let scroller = gtk4::ScrolledWindow::new();
        scroller.set_child(Some(&content));
        scroller.set_vexpand(true);
        let loading = Self::message("Loading…");
        content.append(&loading);
        self.page.append(&scroller);
        if size > MAX_IMAGE_BYTES {
            loading.set_text(&format!("{} is larger than 64 MB.", self.title()));
            return;
        }
        let (weak, path) = (Rc::downgrade(self), self.path.borrow().clone());
        glib::MainContext::default().spawn_local(async move {
            let decoded = gio::spawn_blocking(move || {
                let bytes = std::fs::read(&path).map_err(|e| format!("Cannot read the image: {e}"))?;
                crate::asset_view::frames(&bytes)
            }).await;
            let Some(preview) = weak.upgrade() else { return };
            content.remove(&loading);
            match decoded {
                Ok(Ok(frames)) => {
                    if let Some(first) = frames.first() {
                        preview.set_info(format!("{} × {} · {}", first.width, first.height, size_text(size)));
                    }
                    crate::asset_view::image_preview(&content, frames, Rc::clone(&preview.generation), 0);
                }
                Ok(Err(e)) => content.append(&Self::message(&e)),
                Err(_) => content.append(&Self::message("The image could not be decoded.")),
            }
        });
    }

    /// GTK's player: the video with its controls, or a sound's controls
    /// under its name. Nothing plays until Play.
    fn build_player(self: &Rc<Self>, size: u64) {
        let Some(media) = self.media.clone() else { return };
        let problem = Self::message("");
        problem.set_visible(false);
        if self.kind == Kind::Video {
            let video = gtk4::Video::new();
            video.set_media_stream(Some(&media));
            video.set_autoplay(false);
            video.set_vexpand(true);
            video.set_hexpand(true);
            self.page.append(&video);
        } else {
            let column = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
            column.set_valign(gtk4::Align::Center);
            column.set_vexpand(true);
            let icon = gtk4::Image::from_icon_name("audio-x-generic-symbolic");
            icon.set_pixel_size(96);
            icon.add_css_class("sd-editor-empty");
            column.append(&icon);
            let name = Self::message(&self.title());
            column.append(&name);
            let controls = gtk4::MediaControls::new(Some(&media));
            controls.set_margin_start(24);
            controls.set_margin_end(24);
            column.append(&controls);
            self.page.append(&column);
        }
        self.page.append(&problem);
        // Playback unavailable (often a missing GStreamer plugin): say so,
        // with the packages that add more formats.
        media.connect_error_notify({
            let problem = problem.downgrade();
            move |media| {
                let (Some(problem), Some(error)) = (problem.upgrade(), media.error()) else { return };
                problem.set_text(&format!("This file cannot be played here: {}\nMore formats: {MORE_FORMATS}", plain_error(error.message())));
                problem.set_visible(true);
            }
        });
        media.connect_duration_notify({
            let weak = Rc::downgrade(self);
            move |media| {
                if let (Some(preview), true) = (weak.upgrade(), media.duration() > 0) {
                    preview.set_info(format!("{} · {}", length_text(media.duration()), size_text(size)));
                }
            }
        });
        // A hidden tab or overlay does not keep playing.
        self.page.connect_unmap(move |_| media.pause());
    }

    /// Its file was renamed or moved (the tree's Rename).
    pub fn moved_to(&self, path: &Path) {
        *self.path.borrow_mut() = path.to_path_buf();
        if let Some(media) = &self.media {
            let playing = media.is_playing();
            media.set_filename(Some(path));
            if playing {
                media.play();
            }
        }
        self.refresh_title();
    }

    /// The tab is closing: playback and animation stop.
    pub fn stop(&self) {
        self.generation.set(self.generation.get() + 1);
        if let Some(media) = &self.media {
            media.pause();
            media.clear();
        }
    }

    /// The player, for tests.
    #[cfg(test)]
    pub fn media(&self) -> Option<&gtk4::MediaFile> {
        self.media.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_media_kinds_and_texts() {
        for (name, want) in [
            ("a.png", Kind::Image), ("b.JPG", Kind::Image), ("c.gif", Kind::Image), ("d.webp", Kind::Image), ("e.svg", Kind::Image),
            ("f.mp3", Kind::Audio), ("g.wav", Kind::Audio), ("h.ogg", Kind::Audio), ("i.flac", Kind::Audio), ("j.opus", Kind::Audio),
            ("k.mp4", Kind::Video), ("l.webm", Kind::Video), ("m.mkv", Kind::Video), ("n.mov", Kind::Video), ("o.ogv", Kind::Video),
            ("p.rs", Kind::Text), ("q.md", Kind::Text), ("r.json", Kind::Text), ("Makefile", Kind::Text), ("s.txt", Kind::Text),
        ] {
            assert_eq!(kind(Path::new(name)), want, "{name}");
        }
        assert_eq!(size_text(999), "999 B");
        assert_eq!(size_text(1_500), "2 KB");
        assert_eq!(size_text(2_500_000), "2.5 MB");
        assert_eq!(length_text(65_000_000), "1:05");
        assert_eq!(length_text(3_725_000_000), "1:02:05");
        assert_eq!(plain_error("Error from element /GstPlayBin3:playbin3/GstURIDecodeBin3:uridecodebin3/GstTypeFindElement:typefindelement0: Could not determine type of stream.\n../gstreamer/x.c(1186): loop"), "Could not determine type of stream.");
        assert_eq!(plain_error("no decoder for video/webm"), "no decoder for video/webm.");
    }
}
