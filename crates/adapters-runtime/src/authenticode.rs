use brows_application::{PortError, PortErrorKind, PortResult};
use std::path::Path;

/// Read the executable's native file version, independently of directory names.
pub fn read_file_version(executable: &Path) -> PortResult<String> {
    platform::file_version(executable)
}

/// Verifies the executable's Windows Authenticode trust chain and returns the
/// leaf signing certificate's display name only when it matches the explicit
/// publisher allowlist.
pub fn verify_authenticode_publisher(
    executable: &Path,
    allowed_publishers: &[&str],
) -> PortResult<String> {
    if allowed_publishers.is_empty() {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "publisher allowlist is empty",
        ));
    }

    let publisher = platform::verified_publisher(executable)?;
    if !allowed_publishers
        .iter()
        .any(|allowed| publisher.eq_ignore_ascii_case(allowed.trim()))
    {
        return Err(PortError::new(
            PortErrorKind::RuntimeUntrusted,
            format!("browser executable publisher is not allowed: {publisher}"),
        ));
    }
    Ok(publisher)
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub(super) fn file_version(_executable: &Path) -> PortResult<String> {
        Err(PortError::new(PortErrorKind::Unavailable, "native file versions require Windows"))
    }

    pub(super) fn verified_publisher(_executable: &Path) -> PortResult<String> {
        Err(PortError::new(
            PortErrorKind::Unavailable,
            "Authenticode verification is available only on Windows",
        ))
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    use super::*;
    use std::{ffi::c_void, mem, os::windows::ffi::OsStrExt, ptr};

    #[link(name = "version")]
    extern "system" {
        fn GetFileVersionInfoSizeW(path: *const u16, handle: *mut u32) -> u32;
        fn GetFileVersionInfoW(path: *const u16, handle: u32, size: u32, data: *mut c_void) -> i32;
        fn VerQueryValueW(data: *const c_void, key: *const u16, value: *mut *mut c_void, length: *mut u32) -> i32;
    }

    pub(super) fn file_version(executable: &Path) -> PortResult<String> {
        let path: Vec<u16> = executable.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut ignored = 0;
        // SAFETY: path is a live, NUL-terminated UTF-16 buffer.
        let size = unsafe { GetFileVersionInfoSizeW(path.as_ptr(), &mut ignored) };
        if size == 0 || size > 1024 * 1024 { return Err(untrusted("executable has no bounded file version")); }
        // u32 allocation guarantees alignment for VS_FIXEDFILEINFO.
        let mut data = vec![0_u32; (size as usize).div_ceil(4)];
        // SAFETY: data is aligned and holds at least size bytes.
        if unsafe { GetFileVersionInfoW(path.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
            return Err(untrusted("cannot read executable file version"));
        }
        let key = [b'\\' as u16, 0];
        let mut value = ptr::null_mut();
        let mut length = 0;
        // SAFETY: data is a successful version-info buffer; output pointers live through this call.
        if unsafe { VerQueryValueW(data.as_ptr().cast(), key.as_ptr(), &mut value, &mut length) } == 0 || value.is_null() || length < 52 {
            return Err(untrusted("executable fixed version is missing"));
        }
        // SAFETY: the root query returns a VS_FIXEDFILEINFO of at least 52 bytes.
        let fields = unsafe { std::slice::from_raw_parts(value.cast::<u32>(), 13) };
        if fields[0] != 0xfeef04bd { return Err(untrusted("invalid executable fixed version")); }
        Ok(format!("{}.{}.{}.{}", fields[2] >> 16, fields[2] & 0xffff, fields[3] >> 16, fields[3] & 0xffff))
    }

    const WTD_UI_NONE: u32 = 2;
    const WTD_REVOKE_NONE: u32 = 0;
    const WTD_CHOICE_FILE: u32 = 1;
    const WTD_STATEACTION_VERIFY: u32 = 1;
    const WTD_STATEACTION_CLOSE: u32 = 2;
    const WTD_CACHE_ONLY_URL_RETRIEVAL: u32 = 0x1000;
    const WTD_SAFER_FLAG: u32 = 0x100;
    const WTD_UICONTEXT_EXECUTE: u32 = 0;
    const CERT_NAME_SIMPLE_DISPLAY_TYPE: u32 = 4;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    const WINTRUST_ACTION_GENERIC_VERIFY_V2: Guid = Guid {
        data1: 0x00aac56b,
        data2: 0xcd44,
        data3: 0x11d0,
        data4: [0x8c, 0xc2, 0x00, 0xc0, 0x4f, 0xc2, 0x95, 0xee],
    };

    #[repr(C)]
    struct WinTrustFileInfo {
        cb_struct: u32,
        file_path: *const u16,
        file: *mut c_void,
        known_subject: *mut Guid,
    }

    #[repr(C)]
    struct WinTrustData {
        cb_struct: u32,
        policy_callback_data: *mut c_void,
        sip_client_data: *mut c_void,
        ui_choice: u32,
        revocation_checks: u32,
        union_choice: u32,
        file_info: *mut WinTrustFileInfo,
        state_action: u32,
        state_data: *mut c_void,
        url_reference: *mut u16,
        provider_flags: u32,
        ui_context: u32,
        signature_settings: *mut c_void,
    }

    #[repr(C)]
    struct CryptProviderData {
        _private: [u8; 0],
    }

    #[repr(C)]
    struct CryptProviderSigner {
        _private: [u8; 0],
    }

    #[repr(C)]
    struct CertContext {
        _private: [u8; 0],
    }

    #[repr(C)]
    struct CryptProviderCertPrefix {
        cb_struct: u32,
        cert: *const CertContext,
    }

    #[link(name = "wintrust")]
    unsafe extern "system" {
        fn WinVerifyTrust(
            window: *mut c_void,
            action_id: *mut Guid,
            trust_data: *mut c_void,
        ) -> i32;
        fn WTHelperProvDataFromStateData(state_data: *mut c_void) -> *mut CryptProviderData;
        fn WTHelperGetProvSignerFromChain(
            provider_data: *mut CryptProviderData,
            signer_index: u32,
            counter_signer: i32,
            counter_signer_index: u32,
        ) -> *mut CryptProviderSigner;
        fn WTHelperGetProvCertFromChain(
            signer: *mut CryptProviderSigner,
            cert_index: u32,
        ) -> *mut CryptProviderCertPrefix;
    }

    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CertGetNameStringW(
            cert: *const CertContext,
            name_type: u32,
            flags: u32,
            type_parameter: *const c_void,
            name: *mut u16,
            name_chars: u32,
        ) -> u32;
    }

    pub(super) fn verified_publisher(executable: &Path) -> PortResult<String> {
        if !executable.is_file() {
            return Err(PortError::not_found("browser executable"));
        }
        let wide_path = executable
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut file_info = WinTrustFileInfo {
            cb_struct: size_u32::<WinTrustFileInfo>()?,
            file_path: wide_path.as_ptr(),
            file: ptr::null_mut(),
            known_subject: ptr::null_mut(),
        };
        let mut trust_data = WinTrustData {
            cb_struct: size_u32::<WinTrustData>()?,
            policy_callback_data: ptr::null_mut(),
            sip_client_data: ptr::null_mut(),
            ui_choice: WTD_UI_NONE,
            revocation_checks: WTD_REVOKE_NONE,
            union_choice: WTD_CHOICE_FILE,
            file_info: &mut file_info,
            state_action: WTD_STATEACTION_VERIFY,
            state_data: ptr::null_mut(),
            url_reference: ptr::null_mut(),
            provider_flags: WTD_CACHE_ONLY_URL_RETRIEVAL | WTD_SAFER_FLAG,
            ui_context: WTD_UICONTEXT_EXECUTE,
            signature_settings: ptr::null_mut(),
        };
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;

        // SAFETY: both structures follow WinTrust.h, all borrowed pointers
        // remain alive through the synchronous call, and UI is disabled.
        let status = unsafe {
            WinVerifyTrust(
                ptr::null_mut(),
                &mut action,
                (&mut trust_data as *mut WinTrustData).cast(),
            )
        };
        let state_data = trust_data.state_data;
        // A stateful verification must be closed on both success and failure.
        // WinVerifyTrust can allocate provider state before returning an error.
        let _state_guard = TrustStateGuard {
            action: &mut action,
            data: &mut trust_data,
        };
        if status != 0 {
            return Err(PortError::new(
                PortErrorKind::RuntimeUntrusted,
                format!("browser executable Authenticode verification failed (0x{status:08x})"),
            ));
        }

        // SAFETY: a successful stateful WinVerifyTrust call owns valid
        // provider data until TrustStateGuard closes it.
        let provider = unsafe { WTHelperProvDataFromStateData(state_data) };
        if provider.is_null() {
            return Err(untrusted("verified signature has no provider data"));
        }
        // SAFETY: provider points to the successful verification state.
        let signer = unsafe { WTHelperGetProvSignerFromChain(provider, 0, 0, 0) };
        if signer.is_null() {
            return Err(untrusted("verified signature has no primary signer"));
        }
        // SAFETY: signer is the primary signer from the live provider state.
        let provider_cert = unsafe { WTHelperGetProvCertFromChain(signer, 0) };
        // SAFETY: the helper returns either null or a provider-owned structure
        // whose prefix is cbStruct followed by the certificate pointer.
        let cert = unsafe { provider_cert.as_ref() }
            .map(|value| value.cert)
            .filter(|value| !value.is_null())
            .ok_or_else(|| untrusted("verified signature has no leaf certificate"))?;

        certificate_display_name(cert)
    }

    fn certificate_display_name(cert: *const CertContext) -> PortResult<String> {
        // SAFETY: cert belongs to the live WinVerifyTrust provider state.
        let chars = unsafe {
            CertGetNameStringW(
                cert,
                CERT_NAME_SIMPLE_DISPLAY_TYPE,
                0,
                ptr::null(),
                ptr::null_mut(),
                0,
            )
        };
        if chars <= 1 {
            return Err(untrusted("signing certificate has no publisher name"));
        }
        let capacity = usize::try_from(chars)
            .map_err(|_| untrusted("signing certificate publisher name is too large"))?;
        let mut buffer = vec![0u16; capacity];
        // SAFETY: buffer contains exactly the size requested above and cert
        // remains valid until the verification state is closed.
        let written = unsafe {
            CertGetNameStringW(
                cert,
                CERT_NAME_SIMPLE_DISPLAY_TYPE,
                0,
                ptr::null(),
                buffer.as_mut_ptr(),
                chars,
            )
        };
        if written != chars || buffer.last() != Some(&0) {
            return Err(untrusted("failed to read signing certificate publisher"));
        }
        buffer.pop();
        String::from_utf16(&buffer)
            .map_err(|_| untrusted("signing certificate publisher is not valid UTF-16"))
    }

    fn size_u32<T>() -> PortResult<u32> {
        u32::try_from(mem::size_of::<T>())
            .map_err(|_| PortError::new(PortErrorKind::Internal, "WinTrust structure is too large"))
    }

    fn untrusted(message: &'static str) -> PortError {
        PortError::new(PortErrorKind::RuntimeUntrusted, message)
    }

    struct TrustStateGuard<'a> {
        action: &'a mut Guid,
        data: &'a mut WinTrustData,
    }

    impl Drop for TrustStateGuard<'_> {
        fn drop(&mut self) {
            self.data.state_action = WTD_STATEACTION_CLOSE;
            // SAFETY: this data owns a state handle from the successful verify
            // call and is closed exactly once by this guard.
            unsafe {
                WinVerifyTrust(
                    ptr::null_mut(),
                    self.action,
                    (self.data as *mut WinTrustData).cast(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_publisher_allowlist_is_rejected_before_platform_access() {
        let error = verify_authenticode_publisher(Path::new("missing.exe"), &[]).unwrap_err();
        assert_eq!(error.kind, PortErrorKind::InvalidData);
    }

    #[cfg(windows)]
    #[test]
    fn unsigned_file_is_rejected() {
        let root = std::env::temp_dir().join(format!(
            "brows-unsigned-authenticode-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("unsigned.exe");
        std::fs::write(&file, b"not a signed portable executable").unwrap();
        let error = verify_authenticode_publisher(&file, &["Google LLC"]).unwrap_err();
        assert_eq!(error.kind, PortErrorKind::RuntimeUntrusted);
        std::fs::remove_dir_all(root).unwrap();
    }
}

