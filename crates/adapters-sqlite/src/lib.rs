#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

//! SQLite-backed application adapters.
//!
//! This crate only persists non-secret profile configuration. Proxy
//! credentials and Chromium profile data belong to separate adapters.

mod migration;
mod migration_records;
mod repository;

pub use migration::CURRENT_SCHEMA_VERSION;
pub use migration_records::SqliteMigrationRecordRepository;
pub use repository::SqliteProfileRepository;

#[cfg(test)]
mod tests;
