//! Window-system adapter for the shared GTK overlay.
use gtk4::prelude::*;

/// Invalidate the Mac toplevel after removing a native terminal subtree.
pub fn terminal_removed(widget: &impl IsA<gtk4::Widget>) {
    #[cfg(target_os = "macos")]
    if let Some(window) = widget.root().and_downcast::<gtk4::Window>() {
        // Coalesced by GTK into its next frame; no timer or idle repaint loop.
        window.queue_draw();
        if let Some(surface) = window.surface() {
            surface.queue_render();
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = widget;
}

/// Switch the whole overlay between accepting pointer input and click-through.
pub fn set_pointer_input(window: &impl IsA<gtk4::Window>, enabled: bool) {
    let window = window.as_ref();
    #[cfg(target_os = "macos")]
    if macos::set_native_pointer_input(window, enabled) {
        return;
    }
    if let Some(surface) = window.surface() {
        if enabled {
            surface.set_input_region(None);
        } else {
            surface.set_input_region(Some(&gtk4::cairo::Region::create()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_input_survives_repeated_hide_and_restore() {
        if !crate::gtk_test::is_child() {
            crate::gtk_test::run_in_child_process(
                "desktop_shell::tests::pointer_input_survives_repeated_hide_and_restore",
            );
            return;
        }
        gtk4::init().unwrap();
        let window = gtk4::Window::new();
        // Also exercise calls before a native surface exists.
        set_pointer_input(&window, false);
        set_pointer_input(&window, true);
        window.present();
        for (width, height) in [(480, 360), (800, 600), (400, 300), (800, 600)] {
            assert!(window.surface().is_some());
            set_pointer_input(&window, false);
            window.set_visible(false);
            window.set_default_size(width, height);
            // Regression: the macOS backend crashed here on a null region.
            set_pointer_input(&window, true);
            window.present();
            while gtk4::glib::MainContext::default().pending() {
                gtk4::glib::MainContext::default().iteration(false);
            }
            assert!(window.is_visible());
        }
        window.close();
    }
}

#[cfg(target_os = "linux")]
pub use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

#[cfg(target_os = "linux")]
pub fn before_present(_: &impl gtk4::prelude::IsA<gtk4::Window>) {}

#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "macos")]
mod macos {
    use gtk4::{glib::translate::ToGlibPtr, prelude::*};
    use std::ffi::c_void;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum KeyboardMode {
        None,
        OnDemand,
        Exclusive,
    }
    pub enum Layer {
        Overlay,
    }
    pub enum Edge {
        Top,
        Bottom,
        Left,
        Right,
    }

    extern "C" {
        fn gdk_macos_surface_get_native_window(
            surface: *mut gtk4::gdk::ffi::GdkSurface,
        ) -> *mut c_void;
        fn sd_macos_configure(window: *mut c_void, opaque: bool);
        fn sd_macos_present(window: *mut c_void);
        fn sd_macos_bounds(window: *mut c_void, width: *mut i32, height: *mut i32);
        fn sd_macos_input(window: *mut c_void, enabled: bool);
    }

    fn native(window: &gtk4::Window) -> Option<*mut c_void> {
        let surface = window.surface()?;
        // GTK tests may use Broadway even in a macOS build.
        if !surface.type_().name().starts_with("GdkMacos") {
            return None;
        }
        let pointer = unsafe { gdk_macos_surface_get_native_window(surface.to_glib_none().0) };
        (!pointer.is_null()).then_some(pointer)
    }

    pub(super) fn set_native_pointer_input(window: &gtk4::Window, enabled: bool) -> bool {
        let Some(native) = native(window) else { return false };
        // GTK 4.24's macOS backend dereferences a null input region on restore.
        // AppKit controls whole-window click-through without a region, which
        // also keeps input covering the window after a display-size change.
        unsafe { sd_macos_input(native, enabled) };
        true
    }

    pub fn before_present(window: &impl IsA<gtk4::Window>) {
        let window = window.as_ref();
        gtk4::prelude::WidgetExt::realize(window);
        if let Some(native) = native(window) {
            let (mut width, mut height) = (0, 0);
            unsafe { sd_macos_bounds(native, &mut width, &mut height) };
            if width > 0 && height > 0 {
                window.set_default_size(width, height);
            }
            unsafe { sd_macos_present(native) };
        }
    }

    pub trait LayerShell: IsA<gtk4::Window> {
        fn init_layer_shell(&self) {
            let window = self.as_ref();
            window.set_decorated(false);
            window.set_title(Some("SUPER DESKTOP"));
            window.set_default_size(1000, 700);
            window.connect_realize(|window| {
                if let Some(native) = native(window) {
                    unsafe { sd_macos_configure(native, window.has_css_class("super-desktop-window")) };
                }
            });
        }
        // AppKit handles overlay level and the visible screen rectangle as one
        // operation; these layer-shell settings have no independent Mac state.
        fn set_layer(&self, _: Layer) {}
        fn set_namespace(&self, _: Option<&str>) {}
        fn set_anchor(&self, _: Edge, _: bool) {}
        fn set_keyboard_mode(&self, mode: KeyboardMode) {
            let window = self.as_ref();
            for class in ["sd-keyboard-disabled", "sd-keyboard-exclusive"] {
                window.remove_css_class(class);
            }
            match mode {
                KeyboardMode::None => window.add_css_class("sd-keyboard-disabled"),
                KeyboardMode::Exclusive => window.add_css_class("sd-keyboard-exclusive"),
                KeyboardMode::OnDemand => {}
            }
            if let Some(native) = native(window) {
                unsafe { sd_macos_input(native, mode != KeyboardMode::None) };
            }
        }
        fn keyboard_mode(&self) -> KeyboardMode {
            let window = self.as_ref();
            if window.has_css_class("sd-keyboard-disabled") {
                KeyboardMode::None
            } else if window.has_css_class("sd-keyboard-exclusive") {
                KeyboardMode::Exclusive
            } else {
                KeyboardMode::OnDemand
            }
        }
    }
    impl<T: IsA<gtk4::Window>> LayerShell for T {}
}
