//! Inscribe CLI library module.
//!
//! Provides CLI subcommands, argument definitions, terminal tables, and progress telemetry.

pub mod args;
pub mod commands;
pub mod term;

pub use args::{Cli, Commands, FlashArgs, InspectArgs, ListArgs};
pub use commands::{execute_flash_pipeline, run_flash, run_inspect, run_list};
pub use term::{create_progress_bar, format_drive_list, format_inspection, prompt_unsafe_override};
