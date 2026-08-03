//! Conservative, opt-in host integration for `Code System Graph` routing guidance.
//!
//! The crate installs only marker-owned host configuration. Prompt hooks classify the submitted
//! prompt and return static routing guidance; they never inspect source code or run repository
//! tools. Strict mode additionally installs a Git pre-commit gate that runs a staged `Code System Graph`
//! change analysis.

mod install;
mod managed_root;
mod routing;
mod types;

pub use install::{install, status, uninstall};
pub use routing::{classify_prompt, route};
pub use types::{
    HookError, HookMode, HookStatus, HostKind, InstallReport, InstallRequest, RoutingIntent, RoutingRequest, RoutingResponse, UninstallReport
};
