use crate::{DomainError, DEFAULT_MAX_CONCURRENT_SESSIONS, MAX_CONCURRENT_SESSIONS};

/// Persisted manager preferences. Browser runtime and privacy policies are not mutable settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerSettings {
    pub max_concurrent_sessions: usize,
}

impl Default for ManagerSettings {
    fn default() -> Self {
        Self {
            max_concurrent_sessions: DEFAULT_MAX_CONCURRENT_SESSIONS,
        }
    }
}

impl ManagerSettings {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.max_concurrent_sessions == 0
            || self.max_concurrent_sessions > MAX_CONCURRENT_SESSIONS
        {
            return Err(DomainError::SchedulerLimitInvalid);
        }
        Ok(())
    }
}
