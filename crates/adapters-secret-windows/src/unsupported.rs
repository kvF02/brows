use brows_application::{PortError, PortErrorKind, PortResult, SecretStore, SecretValue};
use brows_domain::SecretRef;
use std::fmt;

#[derive(Clone, Copy, Default)]
pub struct WindowsCredentialStore;

impl WindowsCredentialStore {
    pub const fn new() -> Self {
        Self
    }

    fn unsupported<T>() -> PortResult<T> {
        Err(PortError::new(
            PortErrorKind::Unavailable,
            "Windows Credential Manager is unavailable on this platform",
        ))
    }
}

impl fmt::Debug for WindowsCredentialStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WindowsCredentialStore")
    }
}

impl SecretStore for WindowsCredentialStore {
    fn put(&self, _reference: &SecretRef, _value: &SecretValue) -> PortResult<()> {
        Self::unsupported()
    }

    fn get(&self, _reference: &SecretRef) -> PortResult<SecretValue> {
        Self::unsupported()
    }

    fn delete(&self, _reference: &SecretRef) -> PortResult<()> {
        Self::unsupported()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_fails_explicitly_without_echoing_values() {
        let store = WindowsCredentialStore::new();
        let reference = SecretRef::new("not-windows").unwrap();
        let secret = SecretValue::new(b"do-not-log".to_vec()).unwrap();

        let error = store.put(&reference, &secret).unwrap_err();
        assert_eq!(error.kind, PortErrorKind::Unavailable);
        assert!(!error.message.contains(reference.as_str()));
        assert!(!error.message.contains("do-not-log"));
    }
}
