#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

//! SQLite-backed application adapters.
//!
//! This crate only persists non-secret profile configuration. Proxy
//! credentials and Chromium profile data belong to separate adapters.

mod audit;
mod migration;
mod migration_records;
mod proxies;
mod repository;
mod settings;

pub use audit::SqliteAuditSink;
pub use migration::CURRENT_SCHEMA_VERSION;
pub use migration_records::SqliteMigrationRecordRepository;
pub use proxies::SqliteProxyRepository;
pub use repository::SqliteProfileRepository;
pub use settings::SqliteSettingsRepository;

#[cfg(test)]
mod tests;
