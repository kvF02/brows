use brows_contracts::{ErrorCode, ErrorDto, FieldViolationDto};
use brows_domain::DomainError;
use std::fmt;

pub type ApplicationResult<T> = Result<T, ApplicationError>;

/// Stable application failure that can be mapped directly to the v1 IPC
/// contract without leaking adapter implementation details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationError {
    code: ErrorCode,
    message: String,
    violations: Vec<FieldViolationDto>,
}

impl ApplicationError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            violations: Vec::new(),
        }
    }

    pub fn invalid_field(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::ValidationFailed,
            message: "one or more fields are invalid".to_owned(),
            violations: vec![FieldViolationDto {
                field: field.into(),
                reason: reason.into(),
            }],
        }
    }

    pub fn code(&self) -> &ErrorCode {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn to_contract(&self) -> ErrorDto {
        ErrorDto {
            code: self.code.clone(),
            message: self.message.clone(),
            violations: self.violations.clone(),
        }
    }

    pub(crate) fn poisoned(component: &'static str) -> Self {
        Self::new(
            ErrorCode::Internal,
            format!("{component} state is unavailable"),
        )
    }
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApplicationError {}

impl From<DomainError> for ApplicationError {
    fn from(error: DomainError) -> Self {
        use DomainError::*;
        let code = match &error {
            InvalidStateTransition { .. } | SessionNotActive | SessionAlreadyActive => {
                ErrorCode::InvalidProfileState
            }
            ProfileAlreadyScheduled | DuplicateSession | SessionAlreadyQueued => {
                ErrorCode::AlreadyExists
            }
            SessionNotFound => ErrorCode::NotFound,
            _ => ErrorCode::ValidationFailed,
        };
        Self::new(code, error.to_string())
    }
}

impl From<brows_contracts::ContractError> for ApplicationError {
    fn from(error: brows_contracts::ContractError) -> Self {
        Self::new(ErrorCode::ValidationFailed, error.to_string())
    }
}

impl From<crate::ports::PortError> for ApplicationError {
    fn from(error: crate::ports::PortError) -> Self {
        use crate::ports::PortErrorKind;
        let code = match error.kind {
            PortErrorKind::Cancelled => ErrorCode::OperationCancelled,
            PortErrorKind::NotFound => ErrorCode::NotFound,
            PortErrorKind::AlreadyExists => ErrorCode::AlreadyExists,
            PortErrorKind::RevisionConflict => ErrorCode::RevisionConflict,
            PortErrorKind::Locked => ErrorCode::ProfileLocked,
            PortErrorKind::CapabilityUnsupported => ErrorCode::CapabilityUnsupported,
            PortErrorKind::ProxyConnect => ErrorCode::ProxyConnectFailed,
            PortErrorKind::ProxyAuth => ErrorCode::ProxyAuthFailed,
            PortErrorKind::RuntimeUntrusted => ErrorCode::RuntimeUntrusted,
            PortErrorKind::RuntimeInUse => ErrorCode::RuntimeInUse,
            PortErrorKind::WrongPassword => ErrorCode::BackupWrongPassword,
            PortErrorKind::Tampered => ErrorCode::BackupTampered,
            PortErrorKind::Incompatible => ErrorCode::BackupIncompatible,
            PortErrorKind::InvalidData => ErrorCode::ValidationFailed,
            PortErrorKind::Unavailable | PortErrorKind::Internal => ErrorCode::Internal,
        };
        Self::new(code, error.message)
    }
}
