use crate::credential_target;
use brows_application::{PortError, PortErrorKind, PortResult, SecretStore, SecretValue};
use brows_domain::SecretRef;
use std::{
    fmt,
    sync::atomic::{compiler_fence, Ordering},
};

/// Credential blobs are capped by Windows for Generic Credentials. Rejecting
/// oversized input before FFI also prevents a lossy `usize` to `u32` cast.
const MAX_SECRET_BYTES: usize = 2_560;

#[derive(Clone, Copy, Default)]
pub struct WindowsCredentialStore;

impl WindowsCredentialStore {
    pub const fn new() -> Self {
        Self
    }

    fn target(reference: &SecretRef) -> Vec<u16> {
        credential_target(reference)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }
}

impl fmt::Debug for WindowsCredentialStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WindowsCredentialStore")
    }
}

impl SecretStore for WindowsCredentialStore {
    /// `CredWriteW` replaces an existing Generic Credential with the same
    /// target, making this operation an intentional upsert.
    fn put(&self, reference: &SecretRef, value: &SecretValue) -> PortResult<()> {
        if value.expose().len() > MAX_SECRET_BYTES {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "credential exceeds the Windows Generic Credential size limit",
            ));
        }

        let target = Self::target(reference);
        let mut temporary = SensitiveBuffer::copy_from(value.expose());
        ffi::write(&target, temporary.as_mut_slice())
            .map_err(|error| map_windows_error("write", error, MissingPolicy::Error))
    }

    fn get(&self, reference: &SecretRef) -> PortResult<SecretValue> {
        let target = Self::target(reference);
        let bytes = ffi::read(&target)
            .map_err(|error| map_windows_error("read", error, MissingPolicy::Error))?;
        let temporary = SensitiveBuffer::from_vec(bytes);
        SecretValue::new(temporary.into_vec()).map_err(|_| {
            PortError::new(
                PortErrorKind::InvalidData,
                "credential store returned an empty credential",
            )
        })
    }

    /// Deleting an absent target succeeds so cleanup and rollback are safe to
    /// retry after crashes.
    fn delete(&self, reference: &SecretRef) -> PortResult<()> {
        let target = Self::target(reference);
        ffi::delete(&target)
            .map_err(|error| map_windows_error("delete", error, MissingPolicy::Success))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MissingPolicy {
    Error,
    Success,
}

fn map_windows_error(
    operation: &'static str,
    error: ffi::WindowsError,
    missing: MissingPolicy,
) -> PortError {
    if error.is_not_found() {
        return match missing {
            MissingPolicy::Error => PortError::not_found("credential"),
            // `ffi::delete` consumes this case, so this is defensive only.
            MissingPolicy::Success => PortError::new(
                PortErrorKind::Internal,
                "credential deletion produced an unexpected missing result",
            ),
        };
    }

    let kind = if error.is_invalid_input() {
        PortErrorKind::InvalidData
    } else if error.is_access_or_session_failure() {
        PortErrorKind::Unavailable
    } else {
        PortErrorKind::Internal
    };
    PortError::new(
        kind,
        format!(
            "Windows Credential Manager {operation} failed (error {})",
            error.code()
        ),
    )
}

/// Owns adapter-created secret copies and zeroes them unless ownership is
/// explicitly transferred into the application `SecretValue` return type.
struct SensitiveBuffer {
    bytes: Option<Vec<u8>>,
}

impl SensitiveBuffer {
    fn copy_from(bytes: &[u8]) -> Self {
        Self {
            bytes: Some(bytes.to_vec()),
        }
    }

    fn from_vec(bytes: Vec<u8>) -> Self {
        Self { bytes: Some(bytes) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        self.bytes.as_mut().expect("sensitive buffer is present")
    }

    fn into_vec(mut self) -> Vec<u8> {
        self.bytes.take().expect("sensitive buffer is present")
    }
}

impl fmt::Debug for SensitiveBuffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SensitiveBuffer(<redacted>)")
    }
}

impl Drop for SensitiveBuffer {
    fn drop(&mut self) {
        if let Some(bytes) = &mut self.bytes {
            ffi::secure_zero(bytes);
        }
    }
}

/// The only module allowed to call Win32 APIs or dereference pointers.
///
/// # Safety boundary
///
/// All public functions are safe and require a NUL-terminated target. WinCred
/// calls are synchronous, so borrowed input remains alive for each call.
/// `CredReadW` allocations are guarded, their blob is cleared, and `CredFree`
/// is called exactly once even when validation or copying fails.
#[allow(unsafe_code)]
mod ffi {
    use super::{compiler_fence, Ordering};
    use std::{ffi::c_void, mem, ptr, slice};

    // These declarations mirror WinCred.h and errhandlingapi.h. Keeping the
    // four-function ABI here makes the adapter dependency-free and keeps all
    // platform-specific unsafe code behind one small, reviewable boundary.
    const CRED_TYPE_GENERIC: u32 = 1;
    const CRED_PERSIST_LOCAL_MACHINE: u32 = 2;
    const ERROR_ACCESS_DENIED: u32 = 5;
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const ERROR_NOT_FOUND: u32 = 1_168;
    const ERROR_NO_SUCH_LOGON_SESSION: u32 = 1_312;

    #[repr(C)]
    struct FileTime {
        low_date_time: u32,
        high_date_time: u32,
    }

    #[repr(C)]
    struct CredentialAttributeW {
        keyword: *mut u16,
        flags: u32,
        value_size: u32,
        value: *mut u8,
    }

    #[repr(C)]
    struct CredentialW {
        flags: u32,
        credential_type: u32,
        target_name: *mut u16,
        comment: *mut u16,
        last_written: FileTime,
        credential_blob_size: u32,
        credential_blob: *mut u8,
        persist: u32,
        attribute_count: u32,
        attributes: *mut CredentialAttributeW,
        target_alias: *mut u16,
        user_name: *mut u16,
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn CredWriteW(credential: *const CredentialW, flags: u32) -> i32;
        fn CredReadW(
            target_name: *const u16,
            credential_type: u32,
            flags: u32,
            credential: *mut *mut CredentialW,
        ) -> i32;
        fn CredDeleteW(target_name: *const u16, credential_type: u32, flags: u32) -> i32;
        fn CredFree(buffer: *const c_void);
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLastError() -> u32;
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct WindowsError(u32);

    impl WindowsError {
        pub(super) const fn code(self) -> u32 {
            self.0
        }

        pub(super) const fn is_not_found(self) -> bool {
            self.0 == ERROR_NOT_FOUND
        }

        pub(super) const fn is_invalid_input(self) -> bool {
            self.0 == ERROR_INVALID_PARAMETER
        }

        pub(super) const fn is_access_or_session_failure(self) -> bool {
            self.0 == ERROR_ACCESS_DENIED || self.0 == ERROR_NO_SUCH_LOGON_SESSION
        }
    }

    pub(super) fn write(target: &[u16], blob: &mut [u8]) -> Result<(), WindowsError> {
        debug_assert_eq!(target.last(), Some(&0));
        // SAFETY: `CredentialW` is a C data structure where all-zero is a
        // valid baseline. All pointers below remain valid for the synchronous
        // call and sizes are checked by the caller.
        let mut credential: CredentialW = unsafe { mem::zeroed() };
        credential.credential_type = CRED_TYPE_GENERIC;
        credential.target_name = target.as_ptr().cast_mut();
        credential.credential_blob_size = blob.len() as u32;
        credential.credential_blob = blob.as_mut_ptr();
        credential.persist = CRED_PERSIST_LOCAL_MACHINE;

        // SAFETY: `credential` and its target/blob pointers remain alive until
        // the synchronous WinCred call returns.
        let succeeded = unsafe { CredWriteW(&credential, 0) };
        if succeeded == 0 {
            // SAFETY: called immediately after the failing Win32 operation.
            Err(WindowsError(unsafe { GetLastError() }))
        } else {
            Ok(())
        }
    }

    pub(super) fn read(target: &[u16]) -> Result<Vec<u8>, WindowsError> {
        debug_assert_eq!(target.last(), Some(&0));
        let mut raw = ptr::null_mut::<CredentialW>();
        // SAFETY: target is NUL-terminated and `raw` is a valid out pointer.
        let succeeded = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut raw) };
        if succeeded == 0 {
            // SAFETY: called immediately after the failing Win32 operation.
            return Err(WindowsError(unsafe { GetLastError() }));
        }

        let guard = CredentialGuard(raw);
        // SAFETY: successful `CredReadW` returns a non-null `CREDENTIALW`
        // allocation owned by `guard` until this function returns.
        let credential = unsafe { raw.as_ref() }.ok_or(WindowsError(ERROR_INVALID_PARAMETER))?;
        let size = credential.credential_blob_size as usize;
        if size == 0 || credential.credential_blob.is_null() {
            return Ok(Vec::new());
        }
        // SAFETY: WinCred guarantees the blob contains exactly the reported
        // number of bytes for the lifetime of the credential allocation.
        let bytes = unsafe { slice::from_raw_parts(credential.credential_blob, size) }.to_vec();
        drop(guard);
        Ok(bytes)
    }

    pub(super) fn delete(target: &[u16]) -> Result<(), WindowsError> {
        debug_assert_eq!(target.last(), Some(&0));
        // SAFETY: target is a valid NUL-terminated string for the duration of
        // the synchronous call.
        let succeeded = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
        if succeeded != 0 {
            return Ok(());
        }
        // SAFETY: called immediately after the failing Win32 operation.
        let error = WindowsError(unsafe { GetLastError() });
        if error.is_not_found() {
            Ok(())
        } else {
            Err(error)
        }
    }

    pub(super) fn secure_zero(bytes: &mut [u8]) {
        for byte in bytes {
            // SAFETY: `byte` is a unique mutable reference to initialized
            // memory. Volatile writes prevent the clear from being optimized
            // away before the allocation is released.
            unsafe { ptr::write_volatile(byte, 0) };
        }
        compiler_fence(Ordering::SeqCst);
    }

    struct CredentialGuard(*mut CredentialW);

    impl Drop for CredentialGuard {
        fn drop(&mut self) {
            if self.0.is_null() {
                return;
            }
            // SAFETY: the pointer came from a successful `CredReadW`, remains
            // uniquely owned by this guard, and is freed exactly once below.
            unsafe {
                let credential = &mut *self.0;
                if credential.credential_blob_size > 0 && !credential.credential_blob.is_null() {
                    let blob = slice::from_raw_parts_mut(
                        credential.credential_blob,
                        credential.credential_blob_size as usize,
                    );
                    secure_zero(blob);
                }
                CredFree(self.0.cast_const().cast::<c_void>());
            }
        }
    }
}
