//! Opt-in stage timings; never include note text, terminal output, or credentials.
use std::sync::OnceLock;
use std::time::Instant;

pub fn mark(stage: &str) {
    static START: OnceLock<Option<Instant>> = OnceLock::new();
    if let Some(start) = START.get_or_init(|| {
        (std::env::var_os("SUPER_DESKTOP_PROFILE_STARTUP").as_deref()
            == Some(std::ffi::OsStr::new("1")))
        .then(Instant::now)
    }) {
        eprintln!(
            "SUPER DESKTOP startup +{:.2} ms: {stage}",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}
