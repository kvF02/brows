#![deny(missing_debug_implementations)]
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

//! Current-user Windows Credential Manager adapter for [`SecretStore`].
//!
//! The adapter stores Generic Credentials under a deterministic `Brows/`
//! namespace. `put` replaces an existing value, `get` reports `NotFound`, and
//! `delete` is idempotent. No credential material is formatted into errors or
//! `Debug` output.

use brows_domain::SecretRef;

const TARGET_PREFIX: &str = "Brows/v1/";

#[cfg(not(windows))]
mod unsupported;
#[cfg(windows)]
mod windows;

#[cfg(not(windows))]
pub use unsupported::WindowsCredentialStore;
#[cfg(windows)]
pub use windows::WindowsCredentialStore;

/// Encodes every UTF-8 byte, including otherwise safe-looking ASCII.
///
/// Credential target matching is case-insensitive on Windows. Full bytewise
/// encoding avoids collisions between references that differ only by case,
/// and prevents path separators or punctuation from gaining semantics.
fn credential_target(reference: &SecretRef) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let source = reference.as_str().as_bytes();
    let mut target = String::with_capacity(TARGET_PREFIX.len() + source.len() * 2);
    target.push_str(TARGET_PREFIX);
    for &byte in source {
        target.push(HEX[(byte >> 4) as usize] as char);
        target.push(HEX[(byte & 0x0f) as usize] as char);
    }
    target
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_are_prefixed_separator_safe_and_case_distinct() {
        let upper = credential_target(&SecretRef::new("Proxy/A").unwrap());
        let lower = credential_target(&SecretRef::new("proxy/a").unwrap());

        assert_eq!(upper, "Brows/v1/50726F78792F41");
        assert_eq!(lower, "Brows/v1/70726F78792F61");
        assert_ne!(upper.to_ascii_lowercase(), lower.to_ascii_lowercase());
        assert!(!upper[TARGET_PREFIX.len()..].contains('/'));
    }

    #[test]
    fn unicode_references_are_encoded_by_utf8_byte() {
        let target = credential_target(&SecretRef::new("secret-密").unwrap());
        assert_eq!(target, "Brows/v1/7365637265742DE5AF86");
    }
}
