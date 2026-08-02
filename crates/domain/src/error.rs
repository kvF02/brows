use std::fmt;

/// Errors raised while constructing or changing a domain object.
///
/// The domain crate deliberately exposes a small, stable error type.  Adapters
/// can attach richer diagnostics at their boundary without making the domain
/// depend on a database, GUI toolkit, or operating-system API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    EmptyId { kind: &'static str },
    InvalidId { kind: &'static str, reason: String },
    EmptyName,
    NameTooLong { max: usize },
    InvalidHost,
    InvalidPort,
    InvalidSecretReference,
    InvalidRevision,
    InvalidTimeout,
    InvalidHash,
    InvalidPath,
    InvalidUrl(String),
    InvalidTag(String),
    MissingProxySecret,
    InvalidStateTransition { from: String, to: String },
    DuplicateSession,
    ProfileAlreadyScheduled,
    SessionNotFound,
    SchedulerLimitInvalid,
    SessionNotActive,
    SessionAlreadyQueued,
    SessionAlreadyActive,
    InvalidLaunchArgument(String),
    InconsistentConfiguration(String),
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyId { kind } => write!(f, "{kind} id cannot be empty"),
            Self::InvalidId { kind, reason } => write!(f, "invalid {kind} id: {reason}"),
            Self::EmptyName => f.write_str("name cannot be empty"),
            Self::NameTooLong { max } => write!(f, "name cannot exceed {max} characters"),
            Self::InvalidHost => f.write_str("proxy host is invalid"),
            Self::InvalidPort => f.write_str("proxy port must be between 1 and 65535"),
            Self::InvalidSecretReference => f.write_str("secret reference is invalid"),
            Self::InvalidRevision => f.write_str("revision must be greater than zero"),
            Self::InvalidTimeout => f.write_str("timeout must be greater than zero"),
            Self::InvalidHash => {
                f.write_str("hash must be a 64-character hexadecimal SHA-256 value")
            }
            Self::InvalidPath => f.write_str("path cannot be empty or contain a NUL byte"),
            Self::InvalidUrl(url) => write!(f, "unsupported startup URL: {url}"),
            Self::InvalidTag(tag) => write!(f, "invalid tag: {tag}"),
            Self::MissingProxySecret => f.write_str("a proxy username requires a secret reference"),
            Self::InvalidStateTransition { from, to } => {
                write!(f, "invalid session transition from {from} to {to}")
            }
            Self::DuplicateSession => f.write_str("session is already registered"),
            Self::ProfileAlreadyScheduled => {
                f.write_str("profile already has an active or queued session")
            }
            Self::SessionNotFound => f.write_str("session was not found"),
            Self::SchedulerLimitInvalid => f.write_str("scheduler limit must be between 1 and 10"),
            Self::SessionNotActive => f.write_str("session is not active"),
            Self::SessionAlreadyQueued => f.write_str("session is already queued"),
            Self::SessionAlreadyActive => f.write_str("session is already active"),
            Self::InvalidLaunchArgument(arg) => write!(f, "invalid launch argument: {arg}"),
            Self::InconsistentConfiguration(reason) => {
                write!(f, "inconsistent configuration: {reason}")
            }
        }
    }
}

impl std::error::Error for DomainError {}
