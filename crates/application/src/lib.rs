#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

//! Framework-independent application services for Brows.
//!
//! The crate coordinates the domain model through small, synchronous,
//! object-safe ports. Platform adapters may perform their own asynchronous
//! work behind those ports, while the application API remains deterministic
//! and straightforward to test.

mod compiler;
mod error;
mod migration_service;
pub mod memory;
mod ports;
mod profile_service;
mod session_service;
mod sha256;

#[cfg(test)]
mod tests;

pub use compiler::{LaunchPlanCompiler, PreparedLaunch};
pub use error::{ApplicationError, ApplicationResult};
pub use migration_service::MigrationApplicationService;
pub use ports::*;
pub use profile_service::ProfileApplicationService;
pub use session_service::{SessionApplicationService, SessionLaunchOutcome};
