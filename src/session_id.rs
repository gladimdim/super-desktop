//! Local session-name candidates; the caller still checks the tmux namespace.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn candidate() -> String {
    at(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0))
}

fn at(millis: u128) -> String {
    // The counter guarantees distinct names within this process even with a
    // coarse or backwards-moving clock. PID separates concurrent processes.
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("sd_term_{millis}_{:x}{sequence:016x}", std::process::id())
}

#[cfg(test)]
mod tests {
    #[test]
    fn concurrent_session_names_stay_unique_with_a_frozen_clock() {
        let names = std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..16).map(|_| scope.spawn(|| {
                (0..128).map(|_| super::at(42)).collect::<Vec<_>>()
            })).collect();
            jobs.into_iter().flat_map(|job| job.join().unwrap()).collect::<Vec<_>>()
        });
        assert!(names.iter().all(|name| name.starts_with("sd_term_42_")
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')));
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(unique.len(), 16 * 128);
    }
}
