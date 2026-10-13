//! One plugin surface in the overlay: an `sd-qml-host` process draws the QML
//! off screen, this widget shows its frames and sends it pointer, scroll and
//! key events. A host that fails shows an error in place; a host that
//! crashes is restarted with backoff.
use super_desktop::plugins::host::{HostProcess, SharedBuffer, SpawnOptions};
use super_desktop::plugins::protocol::{self, FromHost};
use super_desktop::plugins::paths;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use gtk4::{gdk, glib};
use gtk4::glib::translate::IntoGlib;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SurfaceView {
        pub texture: RefCell<Option<gdk::Texture>>,
        pub on_allocate: RefCell<Option<Box<dyn Fn(i32, i32)>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SurfaceView {
        const NAME: &'static str = "SdPluginSurfaceView";
        type Type = super::SurfaceView;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for SurfaceView {}

    impl WidgetImpl for SurfaceView {
        fn snapshot(&self, snapshot: &gtk4::Snapshot) {
            if let Some(texture) = self.texture.borrow().as_ref() {
                let widget = self.obj();
                let bounds = gtk4::graphene::Rect::new(0.0, 0.0, widget.width() as f32, widget.height() as f32);
                snapshot.append_texture(texture, &bounds);
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            if let Some(callback) = self.on_allocate.borrow().as_ref() {
                callback(width, height);
            }
        }
    }
}

glib::wrapper! {
    /// Draws the latest frame of a plugin surface, stretched to its allocation
    /// (the frame is rendered at that size times the output scale).
    pub struct SurfaceView(ObjectSubclass<imp::SurfaceView>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl SurfaceView {
    fn new() -> Self {
        glib::Object::new()
    }

    fn set_texture(&self, texture: gdk::Texture) {
        self.imp().texture.replace(Some(texture));
        self.queue_draw();
    }

    fn clear(&self) {
        self.imp().texture.replace(None);
        self.queue_draw();
    }
}

/// What a surface shows: which plugin, which QML file.
#[derive(Clone, Debug)]
pub struct SurfaceSpec {
    pub plugin_id: String,
    pub plugin_name: String,
    pub surface_id: String,
    pub qml: PathBuf,
    pub dir: PathBuf,
}

type Packet = (Vec<u8>, Option<std::os::fd::OwnedFd>);

const CRASH_WINDOW: Duration = Duration::from_secs(10);
const CRASH_LIMIT: usize = 3;

struct Inner {
    spec: SurfaceSpec,
    container: gtk4::Overlay,
    view: SurfaceView,
    error: gtk4::Label,
    host: RefCell<Option<HostProcess>>,
    generation: Cell<u64>,
    buffer: RefCell<Option<SharedBuffer>>,
    size: Cell<(i32, i32)>,
    scale: Cell<f64>,
    pointer: Cell<(f64, f64)>,
    failed: Cell<bool>,
    crashes: RefCell<Vec<Instant>>,
    restart: RefCell<Option<glib::SourceId>>,
    disposed: Cell<bool>,
}

pub struct QmlSurface {
    inner: Rc<Inner>,
}

thread_local! {
    static LIVE: RefCell<Vec<Weak<Inner>>> = const { RefCell::new(Vec::new()) };
    static THEME: RefCell<Value> = RefCell::new(json!({}));
}

/// Sends a new theme to every running surface and remembers it for new ones
/// and for `plugin snapshot`.
pub fn set_theme(theme: Value) {
    let _ = std::fs::create_dir_all(paths::state_dir());
    let _ = std::fs::write(paths::theme_path(), theme.to_string());
    THEME.with(|cell| *cell.borrow_mut() = theme.clone());
    LIVE.with(|live| {
        live.borrow_mut().retain(|weak| weak.strong_count() > 0);
        for inner in live.borrow().iter().filter_map(Weak::upgrade) {
            inner.send(&protocol::theme(&theme));
        }
    });
}

fn current_theme() -> Value {
    THEME.with(|cell| cell.borrow().clone())
}

impl QmlSurface {
    pub fn new(spec: SurfaceSpec) -> Self {
        let view = SurfaceView::new();
        view.set_hexpand(true);
        view.set_vexpand(true);
        view.set_focusable(true);
        view.set_focus_on_click(true);
        view.add_css_class("plugin-surface");

        let error = gtk4::Label::new(None);
        error.add_css_class("plugin-surface-error");
        error.set_wrap(true);
        error.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
        error.set_valign(gtk4::Align::Start);
        error.set_xalign(0.0);
        error.set_selectable(true);
        error.set_visible(false);

        let container = gtk4::Overlay::new();
        container.set_child(Some(&view));
        container.add_overlay(&error);
        container.set_measure_overlay(&error, false);
        container.set_clip_overlay(&error, true);
        container.set_overflow(gtk4::Overflow::Hidden);
        container.set_tooltip_text(Some(&spec.plugin_name));

        let inner = Rc::new(Inner {
            spec,
            container,
            view,
            error,
            host: RefCell::new(None),
            generation: Cell::new(0),
            buffer: RefCell::new(None),
            size: Cell::new((0, 0)),
            scale: Cell::new(1.0),
            pointer: Cell::new((0.0, 0.0)),
            failed: Cell::new(false),
            crashes: RefCell::new(Vec::new()),
            restart: RefCell::new(None),
            disposed: Cell::new(false),
        });
        LIVE.with(|live| live.borrow_mut().push(Rc::downgrade(&inner)));
        Inner::bind(&inner);
        Self { inner }
    }

    pub fn widget(&self) -> &gtk4::Overlay {
        &self.inner.container
    }

    /// Ends the host. The widget can be dropped afterwards.
    pub fn dispose(&self) {
        self.inner.disposed.set(true);
        self.inner.stop();
        if let Some(id) = self.inner.restart.take() {
            id.remove();
        }
    }
}

impl Drop for QmlSurface {
    fn drop(&mut self) {
        self.dispose();
    }
}

impl Inner {
    fn bind(this: &Rc<Self>) {
        let weak = Rc::downgrade(this);
        *this.view.imp().on_allocate.borrow_mut() = Some(Box::new({
            let weak = weak.clone();
            move |width, height| {
                if let Some(inner) = weak.upgrade() {
                    inner.allocated(width, height);
                }
            }
        }));
        this.view.connect_map({
            let weak = weak.clone();
            move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.send(&protocol::visible(true));
                }
            }
        });
        this.view.connect_unmap({
            let weak = weak.clone();
            move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.send(&protocol::visible(false));
                }
            }
        });
        Self::bind_input(this);
    }

    fn bind_input(this: &Rc<Self>) {
        let weak = Rc::downgrade(this);
        let mods = |controller: &gtk4::EventController| {
            protocol::qt_modifiers(controller.current_event_state().bits())
        };

        let motion = gtk4::EventControllerMotion::new();
        motion.connect_enter({
            let weak = weak.clone();
            move |c, x, y| {
                if let Some(inner) = weak.upgrade() {
                    inner.pointer.set((x, y));
                    inner.send(&protocol::pointer("enter", x, y, 0, mods(c.upcast_ref())));
                }
            }
        });
        motion.connect_motion({
            let weak = weak.clone();
            move |c, x, y| {
                if let Some(inner) = weak.upgrade() {
                    inner.pointer.set((x, y));
                    inner.send(&protocol::pointer("move", x, y, 0, mods(c.upcast_ref())));
                }
            }
        });
        motion.connect_leave({
            let weak = weak.clone();
            move |_| {
                if let Some(inner) = weak.upgrade() {
                    let (x, y) = inner.pointer.get();
                    inner.send(&protocol::pointer("leave", x, y, 0, 0));
                }
            }
        });
        this.view.add_controller(motion);

        let click = gtk4::GestureClick::new();
        click.set_button(0);
        click.connect_pressed({
            let weak = weak.clone();
            move |gesture, n_press, x, y| {
                if let Some(inner) = weak.upgrade() {
                    inner.view.grab_focus();
                    let button = protocol::qt_button(gesture.current_button());
                    let kind = if n_press == 2 { "double" } else { "press" };
                    inner.send(&protocol::pointer(kind, x, y, button, mods(gesture.upcast_ref())));
                }
            }
        });
        click.connect_released({
            let weak = weak.clone();
            move |gesture, _, x, y| {
                if let Some(inner) = weak.upgrade() {
                    let button = protocol::qt_button(gesture.current_button());
                    inner.send(&protocol::pointer("release", x, y, button, mods(gesture.upcast_ref())));
                }
            }
        });
        this.view.add_controller(click);

        let scroll = gtk4::EventControllerScroll::new(gtk4::EventControllerScrollFlags::BOTH_AXES);
        scroll.connect_scroll({
            let weak = weak.clone();
            move |c, dx, dy| {
                if let Some(inner) = weak.upgrade() {
                    let (x, y) = inner.pointer.get();
                    inner.send(&protocol::scroll(x, y, dx, dy, mods(c.upcast_ref())));
                }
                glib::Propagation::Stop
            }
        });
        this.view.add_controller(scroll);

        let keys = gtk4::EventControllerKey::new();
        keys.connect_key_pressed({
            let weak = weak.clone();
            move |c, keyval, _, state| {
                if let Some(inner) = weak.upgrade() {
                    let text = keyval.to_unicode().filter(|ch| !ch.is_control()).map(String::from).unwrap_or_default();
                    inner.send(&protocol::key(true, protocol::qt_key(keyval.into_glib()), &text, protocol::qt_modifiers(state.bits())));
                    let _ = c;
                }
                // Escape still closes the overlay.
                if keyval == gdk::Key::Escape {
                    glib::Propagation::Proceed
                } else {
                    glib::Propagation::Stop
                }
            }
        });
        keys.connect_key_released({
            let weak = weak.clone();
            move |_, keyval, _, state| {
                if let Some(inner) = weak.upgrade() {
                    inner.send(&protocol::key(false, protocol::qt_key(keyval.into_glib()), "", protocol::qt_modifiers(state.bits())));
                }
            }
        });
        this.view.add_controller(keys);

        let focus = gtk4::EventControllerFocus::new();
        focus.connect_enter({
            let weak = weak.clone();
            move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.send(&protocol::focus(true));
                }
            }
        });
        focus.connect_leave({
            let weak = weak.clone();
            move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.send(&protocol::focus(false));
                }
            }
        });
        this.view.add_controller(focus);
    }

    fn output_scale(&self) -> f64 {
        self.view
            .native()
            .and_then(|native| native.surface())
            .map(|surface| surface.scale())
            .filter(|scale| *scale > 0.0)
            .unwrap_or_else(|| f64::from(self.view.scale_factor().max(1)))
    }

    fn allocated(self: &Rc<Self>, width: i32, height: i32) {
        if width <= 0 || height <= 0 || self.disposed.get() {
            return;
        }
        let scale = self.output_scale();
        let previous = self.size.replace((width, height));
        let scale_changed = (self.scale.replace(scale) - scale).abs() > 0.001;
        if self.host.borrow().is_none() {
            if !self.failed.get() && self.restart.borrow().is_none() {
                self.start();
            }
        } else if scale_changed {
            // Qt takes the scale at start-up only.
            self.stop();
            self.start();
        } else if previous != (width, height) {
            self.send(&protocol::resize(width, height));
        }
    }

    fn log(&self, line: &str) {
        paths::append_log(&self.spec.plugin_id, &format!("[{}] {line}", self.spec.surface_id));
    }

    fn send(&self, message: &Value) {
        if let Some(host) = self.host.borrow().as_ref() {
            let _ = host.send(message);
        }
    }

    fn show_error(&self, text: &str) {
        self.error.set_text(text);
        self.error.set_visible(true);
        self.view.clear();
    }

    fn start(self: &Rc<Self>) {
        let Some(binary) = paths::qml_host_binary() else {
            self.failed.set(true);
            let message = "sd-qml-host is not installed: install qt6-declarative and update SUPER DESKTOP.";
            self.log(message);
            self.show_error(&format!("{}\n{message}", self.spec.plugin_name));
            paths::write_status(&self.spec.plugin_id, "error", &json!([{ "message": message }]));
            return;
        };
        let (width, height) = self.size.get();
        let theme = current_theme();
        let log = paths::log_path(&self.spec.plugin_id);
        if let Some(dir) = log.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let options = SpawnOptions {
            host: &binary,
            qml: &self.spec.qml,
            plugin_dir: &self.spec.dir,
            plugin_id: &self.spec.plugin_id,
            surface_id: &self.spec.surface_id,
            width: width.max(1),
            height: height.max(1),
            scale: self.scale.get(),
            theme: &theme,
            log: Some(log),
            data: Some(paths::data_path(&self.spec.plugin_id)),
        };
        match HostProcess::spawn(&options) {
            Ok(host) => {
                self.log(&format!("started {} (pid {})", self.spec.qml.display(), host.pid()));
                let reader = match host.reader() {
                    Ok(reader) => reader,
                    Err(e) => {
                        self.failed.set(true);
                        self.show_error(&format!("{}\ncannot talk to the QML host: {e}", self.spec.plugin_name));
                        return;
                    }
                };
                *self.host.borrow_mut() = Some(host);
                let generation = self.generation.get().wrapping_add(1);
                self.generation.set(generation);
                // Blocking reads on a thread; handled on the GTK thread.
                let (tx, mut rx) = futures_channel::mpsc::unbounded::<Option<Packet>>();
                std::thread::Builder::new()
                    .name(format!("sd-plugin-{}", self.spec.plugin_id))
                    .spawn(move || loop {
                        match reader.next() {
                            Ok(packet) => {
                                if tx.unbounded_send(Some(packet)).is_err() {
                                    return;
                                }
                            }
                            Err(_) => {
                                let _ = tx.unbounded_send(None);
                                return;
                            }
                        }
                    })
                    .ok();
                let weak = Rc::downgrade(self);
                glib::spawn_future_local(async move {
                    use futures_util::StreamExt;
                    while let Some(packet) = rx.next().await {
                        let Some(inner) = weak.upgrade() else { return };
                        if inner.generation.get() != generation {
                            return;
                        }
                        match packet {
                            Some((bytes, fd)) => inner.handle(protocol::parse(&bytes), fd),
                            None => {
                                inner.exited();
                                return;
                            }
                        }
                    }
                });
            }
            Err(e) => {
                self.failed.set(true);
                self.log(&format!("cannot start {}: {e}", binary.display()));
                self.show_error(&format!("{}\ncannot start the QML host: {e}", self.spec.plugin_name));
            }
        }
    }

    /// Stops the host without treating it as a crash.
    fn stop(&self) {
        // Messages still queued from this host are dropped.
        self.generation.set(self.generation.get().wrapping_add(1));
        self.host.take();
        self.buffer.take();
    }

    fn handle(self: &Rc<Self>, message: FromHost, fd: Option<std::os::fd::OwnedFd>) {
        match message {
            FromHost::Ready => {
                self.failed.set(false);
                self.error.set_visible(false);
                let (width, height) = self.size.get();
                self.send(&protocol::resize(width, height));
                self.send(&protocol::visible(self.view.is_mapped()));
                paths::write_status(&self.spec.plugin_id, "running", &json!([]));
            }
            FromHost::Error(errors) => {
                self.failed.set(true);
                let first = errors.get(0).cloned().unwrap_or(Value::Null);
                let file = first["file"].as_str().unwrap_or("");
                let file = std::path::Path::new(file).file_name().and_then(|n| n.to_str()).unwrap_or(file);
                let text = format!(
                    "{} failed to load\n{}:{}: {}\n\nsuper-desktop plugin check {}",
                    self.spec.plugin_name, file, first["line"], first["message"].as_str().unwrap_or(""), self.spec.plugin_id
                );
                for error in errors.as_array().into_iter().flatten() {
                    self.log(&format!("error: {}:{}:{}: {}", error["file"].as_str().unwrap_or(""), error["line"], error["column"], error["message"].as_str().unwrap_or("")));
                }
                self.show_error(&text);
                paths::write_status(&self.spec.plugin_id, "error", &errors);
            }
            FromHost::Buffer { width, height, stride, scale } => match fd.map(|fd| SharedBuffer::map(fd, width, height, stride, scale)) {
                Some(Ok(buffer)) => *self.buffer.borrow_mut() = Some(buffer),
                Some(Err(e)) => self.log(&format!("bad frame buffer: {e}")),
                None => self.log("frame buffer without a file descriptor"),
            },
            FromHost::Frame { seq } => {
                if let Some(buffer) = self.buffer.borrow().as_ref() {
                    let bytes = glib::Bytes::from(buffer.bytes());
                    let texture = gdk::MemoryTexture::new(
                        buffer.width,
                        buffer.height,
                        gdk::MemoryFormat::B8g8r8a8Premultiplied,
                        &bytes,
                        buffer.stride as usize,
                    );
                    self.view.set_texture(texture.upcast());
                }
                self.send(&protocol::ack(seq));
            }
            FromHost::Log { level, message } => self.log(&format!("{level}: {message}")),
            FromHost::Call { id, method, params } => {
                let result = super::api::call(&self.spec, &method, &params);
                self.send(&protocol::reply(id, result));
            }
            FromHost::Cursor { shape } => {
                self.view.set_cursor_from_name(protocol::cursor_name(shape));
            }
            FromHost::Unknown => {}
        }
    }

    fn exited(self: &Rc<Self>) {
        let status = self.host.borrow_mut().as_mut().and_then(HostProcess::exited);
        self.host.take();
        self.buffer.take();
        if self.disposed.get() {
            return;
        }
        if self.failed.get() {
            // A load error: wait for the next file change.
            return;
        }
        let now = Instant::now();
        let mut crashes = self.crashes.borrow_mut();
        crashes.retain(|at| now.duration_since(*at) < CRASH_WINDOW);
        crashes.push(now);
        let count = crashes.len();
        drop(crashes);
        let how = status.map_or_else(|| "ended".to_owned(), |s| format!("ended ({s})"));
        self.log(&format!("QML host {how}"));
        if count >= CRASH_LIMIT {
            self.failed.set(true);
            self.show_error(&format!(
                "{} stopped after crashing {CRASH_LIMIT} times.\nSave a file of the plugin to try again.\n\nsuper-desktop plugin logs {}",
                self.spec.plugin_name, self.spec.plugin_id
            ));
            paths::write_status(&self.spec.plugin_id, "error", &json!([{ "message": format!("crashed {CRASH_LIMIT} times") }]));
            return;
        }
        let delay = Duration::from_secs(1 << (count - 1).min(4));
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(delay, move || {
            if let Some(inner) = weak.upgrade() {
                inner.restart.take();
                if !inner.disposed.get() && inner.host.borrow().is_none() {
                    inner.start();
                }
            }
        });
        *self.restart.borrow_mut() = Some(id);
    }
}
