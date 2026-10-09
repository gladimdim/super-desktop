//! Shared services that do not require a display or load GTK.
pub mod cli;
pub mod cli_legacy;
pub mod control;
pub mod control_journal;
pub mod control_output;
pub mod harness_record;
pub mod mcp_settings;
pub mod platform;
pub mod preload;
pub mod session_id;
pub mod session_task;
pub mod terminal_text;

// Command families of the structured CLI, dispatched from `cli`.
mod cli_admin;
mod cli_application;
mod cli_attach;
mod cli_connection;
mod cli_extended;
mod cli_files;
mod cli_launch;
mod cli_launch_flow;
mod cli_local;
mod cli_peer;
mod cli_preferences;
mod cli_viewport;
mod cli_workspace;
