//! Optional Bring-Your-Own-Bot (BYOB) Telegram integration with Forum Topics.
//!
//! Each user creates their own bot via `@BotFather` and enters the token on
//! this PC. The detached `telegram-bot` worker makes outbound HTTPS long-poll
//! requests directly to `api.telegram.org` — no relay server sits in between.

pub mod api;
pub mod config;
pub mod desktop;
pub mod notifier;
pub mod pairing;
pub mod process;
pub mod redact;
pub mod router;
pub mod topics;
pub mod worker;

#[cfg(test)]
mod tests;

pub use process::{start_worker, stop_worker, supervise, supervise_while, worker_running};
pub use worker::{serve, verify_token};
