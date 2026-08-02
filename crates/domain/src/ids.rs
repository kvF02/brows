use crate::DomainError;
use std::{fmt, str::FromStr};

macro_rules! typed_id {
    ($name:ident, $kind:literal) => {
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(DomainError::EmptyId { kind: $kind });
                }
                if value.len() > 128 {
                    return Err(DomainError::InvalidId {
                        kind: $kind,
                        reason: "must not exceed 128 bytes".to_owned(),
                    });
                }
                if value.bytes().any(|b| b == 0 || b.is_ascii_whitespace()) {
                    return Err(DomainError::InvalidId {
                        kind: $kind,
                        reason: "must not contain whitespace or NUL".to_owned(),
                    });
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_tuple($kind).field(&self.0).finish()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = DomainError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }
    };
}

typed_id!(ProfileId, "profile");
typed_id!(ProxyId, "proxy");
typed_id!(BrowserBuildId, "browser build");
typed_id!(PresetId, "preset");
typed_id!(SessionId, "session");
typed_id!(SecretRef, "secret reference");

/// A monotonically increasing profile configuration revision used for
/// optimistic concurrency checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(u64);

impl Revision {
    pub const FIRST: Self = Self(1);

    pub fn new(value: u64) -> Result<Self, DomainError> {
        if value == 0 {
            Err(DomainError::InvalidRevision)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1).max(1))
    }
}

impl Default for Revision {
    fn default() -> Self {
        Self::FIRST
    }
}
