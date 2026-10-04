//! Native key-release shortcut; no Accessibility or input-monitoring permission.
use std::{cell::RefCell, ffi::CString, rc::Rc};

thread_local! {
    static ACTION: RefCell<Option<Rc<dyn Fn()>>> = RefCell::new(None);
    static QUIT: RefCell<Option<Rc<dyn Fn()>>> = RefCell::new(None);
    static REGISTERED: RefCell<Option<(u32, u32)>> = RefCell::new(None);
}
extern "C" {
    fn sd_macos_hotkey(key: u32, modifiers: u32) -> i32;
    fn sd_macos_capture_shortcut(capture: bool) -> i32;
    fn sd_macos_setup(toggle: extern "C" fn(), quit: extern "C" fn());
    fn sd_macos_shortcut_status(message: *const std::ffi::c_char);
}

extern "C" fn fired() {
    // Defer until GTK is out of the native event callback before mapping UI.
    ACTION.with(|action| {
        if let Some(action) = action.borrow().clone() {
            gtk4::glib::idle_add_local_once(move || action());
        }
    });
}

extern "C" fn quit() {
    QUIT.with(|action| {
        if let Some(action) = action.borrow().clone() {
            gtk4::glib::idle_add_local_once(move || action());
        }
    });
}

pub fn install(combo: &str, action: impl Fn() + 'static, on_quit: impl Fn() + 'static) -> Result<(), String> {
    ACTION.with(|stored| *stored.borrow_mut() = Some(Rc::new(action)));
    QUIT.with(|stored| *stored.borrow_mut() = Some(Rc::new(on_quit)));
    // Keep the menu available even if a saved key cannot be registered.
    unsafe { sd_macos_setup(fired, quit) };
    let result = apply(combo);
    if result.is_err() {
        set_status("Shortcut unavailable — choose one in Settings");
    }
    result
}

fn set_status(message: &str) {
    if let Ok(message) = CString::new(message) {
        unsafe { sd_macos_shortcut_status(message.as_ptr()) };
    }
}

pub fn apply(combo: &str) -> Result<(), String> {
    let keys = crate::platform::macos_key::parse(combo)?;
    if REGISTERED.with(|registered| *registered.borrow() == Some(keys)) {
        return Ok(());
    }
    let result = unsafe { sd_macos_hotkey(keys.0, keys.1) };
    if result != 0 {
        return Err(format!(
            "macOS could not register {combo} (error {result}); choose another combination"
        ));
    }
    REGISTERED.with(|registered| *registered.borrow_mut() = Some(keys));
    set_status(&format!("Shortcut: {combo}"));
    Ok(())
}

pub fn capture(active: bool) {
    let result = unsafe { sd_macos_capture_shortcut(active) };
    if result != 0 {
        // Allow reapplying the same combo after a failed native restoration.
        REGISTERED.with(|registered| *registered.borrow_mut() = None);
        set_status("Shortcut unavailable — choose one in Settings");
        crate::crashlog::note(&format!("shortcut registration failed: restoring capture ({result})"));
    }
}
