#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

//! Framework-independent application services for Brows.
//!
//! The crate coordinates the domain model through small, synchronous,
//! object-safe ports. Platform adapters may perform their own asynchronous
//! work behind those ports, while the application API remains deterministic
//! and straightforward to test.

mod backup_service;
mod compiler;
mod error;
pub mod memory;
mod migration_service;
pub mod persona_fingerprint;
mod ports;
mod profile_operations;
mod profile_service;
mod proxy_service;
mod session_service;
mod settings_service;
mod sha256;
#[path = "stealth_template/mod.rs"]
mod stealth_template;

#[cfg(test)]
mod tests;

pub use backup_service::BackupApplicationService;
pub use compiler::{LaunchPlanCompiler, PreparedLaunch};
pub use error::{ApplicationError, ApplicationResult};
pub use migration_service::MigrationApplicationService;
pub use ports::*;
pub use profile_operations::{ProfileOperations, ProfileReservation};
pub use profile_service::ProfileApplicationService;
pub use proxy_service::ProxyApplicationService;
pub use session_service::{SessionApplicationService, SessionLaunchOutcome};
pub use settings_service::SettingsApplicationService;
