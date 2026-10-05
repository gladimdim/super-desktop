//! Shared services that do not require a display or load GTK.
pub mod harness_record;
pub mod platform;
pub mod session_task;
pub mod session_id;
pub mod terminal_text;
pub mod cli;
pub mod control;

mod cli_extended;

mod cli_workspace;

mod cli_preferences;

mod cli_files;

mod cli_viewport;

mod cli_attach;

mod cli_admin;

mod cli_launch;

pub mod control_journal;
mod cli_application;

mod cli_peer;

mod cli_launch_flow;

pub mod control_output;
