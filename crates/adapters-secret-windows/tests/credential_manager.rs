#![cfg(windows)]

use brows_adapters_secret_windows::WindowsCredentialStore;
use brows_application::{PortErrorKind, SecretStore, SecretValue};
use brows_domain::SecretRef;
use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash, Hasher},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

struct Cleanup<'a> {
    store: &'a WindowsCredentialStore,
    reference: &'a SecretRef,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = self.store.delete(self.reference);
    }
}

fn random_reference() -> SecretRef {
    fn random_word(salt: u64) -> u64 {
        let mut hasher = RandomState::new().build_hasher();
        process::id().hash(&mut hasher);
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after Unix epoch")
            .as_nanos()
            .hash(&mut hasher);
        salt.hash(&mut hasher);
        hasher.finish()
    }

    SecretRef::new(format!(
        "integration-{:016x}{:016x}",
        random_word(0),
        random_word(1)
    ))
    .expect("generated reference is valid")
}

#[test]
fn generic_credential_round_trip_overwrite_and_idempotent_delete() {
    let store = WindowsCredentialStore::new();
    let reference = random_reference();
    let _cleanup = Cleanup {
        store: &store,
        reference: &reference,
    };

    let first = SecretValue::new(b"first-secret".to_vec()).unwrap();
    store.put(&reference, &first).unwrap();
    assert_eq!(store.get(&reference).unwrap(), first);

    let replacement = SecretValue::new(vec![0, 1, 2, 0xff, 0x7f]).unwrap();
    store.put(&reference, &replacement).unwrap();
    assert_eq!(store.get(&reference).unwrap(), replacement);

    store.delete(&reference).unwrap();
    store.delete(&reference).unwrap();
    assert_eq!(
        store.get(&reference).unwrap_err().kind,
        PortErrorKind::NotFound
    );
}

#[test]
fn oversized_generic_credential_is_rejected_without_side_effects() {
    let store = WindowsCredentialStore::new();
    let reference = random_reference();
    let _cleanup = Cleanup {
        store: &store,
        reference: &reference,
    };

    let maximum = SecretValue::new(vec![0x41; 2_560]).unwrap();
    store.put(&reference, &maximum).unwrap();
    assert_eq!(store.get(&reference).unwrap(), maximum);
    store.delete(&reference).unwrap();

    let oversized = SecretValue::new(vec![0x5a; 2_561]).unwrap();
    let error = store.put(&reference, &oversized).unwrap_err();

    assert_eq!(error.kind, PortErrorKind::InvalidData);
    assert_eq!(
        store.get(&reference).unwrap_err().kind,
        PortErrorKind::NotFound
    );
}
