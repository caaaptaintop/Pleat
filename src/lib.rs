pub mod adoption;
pub mod agents;
pub mod config;
pub mod cross_project;
mod directory_link;
pub mod effective_skills;
mod config_read;
pub use agents::grok::manual::{Invocation as GrokInvocation, invocation as grok_invocation};
pub mod library;
pub mod model;
pub mod project;
pub mod project_discovery;
pub mod proma;
pub mod proma_batch;
pub mod source;
pub mod tags;
pub mod text;
mod trigger;
mod update_protection;
mod package_fs;
mod host_instructions;
mod native_adaptation_reader;
mod native_grok_adaptation_hook;
#[path = "host_instructions/permissions.rs"]
mod file_access;

#[cfg(feature = "desktop")]
pub mod assets;
#[cfg(feature = "desktop")]
mod platform;
#[cfg(feature = "desktop")]
pub mod ui;

pub use config::AppConfig;
pub use library::SkillLibrary;
pub use model::*;

/// Private host-extension protocol; dispatch before starting a CLI or window.
pub fn run_native_adaptation_reader_if_requested() {
    native_grok_adaptation_hook::run_if_requested();
    native_adaptation_reader::run_if_requested();
}
