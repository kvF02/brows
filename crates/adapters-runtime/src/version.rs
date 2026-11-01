use super::*;

#[cfg(not(windows))]
pub(super) fn file_version(_path: &Path) -> PortResult<[u32; 4]> {
    Err(PortError::new(
        PortErrorKind::CapabilityUnsupported,
        "浏览器运行时安装仅支持 Windows",
    ))
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(super) fn file_version(path: &Path) -> PortResult<[u32; 4]> {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt};
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct FixedInfo {
        signature: u32,
        structure_version: u32,
        file_ms: u32,
        file_ls: u32,
        product_ms: u32,
        product_ls: u32,
        flags_mask: u32,
        flags: u32,
        os: u32,
        kind: u32,
        subtype: u32,
        date_ms: u32,
        date_ls: u32,
    }
    #[link(name = "version")]
    unsafe extern "system" {
        fn GetFileVersionInfoSizeW(path: *const u16, handle: *mut u32) -> u32;
        fn GetFileVersionInfoW(path: *const u16, handle: u32, size: u32, data: *mut c_void) -> i32;
        fn VerQueryValueW(
            data: *const c_void,
            key: *const u16,
            value: *mut *mut c_void,
            size: *mut u32,
        ) -> i32;
    }
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut ignored = 0u32;
    // SAFETY: terminated wide path and valid output pointers live through each
    // synchronous Win32 call. Returned data is bounds-checked before copying.
    let size = unsafe { GetFileVersionInfoSizeW(path.as_ptr(), &mut ignored) };
    if size == 0 || size > 1024 * 1024 {
        return Err(bundle::untrusted("无法读取浏览器实际版本"));
    }
    let mut data = vec![0u8; size as usize];
    if unsafe { GetFileVersionInfoW(path.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
        return Err(bundle::untrusted("浏览器版本资源无效"));
    }
    let mut info = std::ptr::null_mut();
    let mut bytes = 0u32;
    if unsafe {
        VerQueryValueW(
            data.as_ptr().cast(),
            [b'\\' as u16, 0].as_ptr(),
            &mut info,
            &mut bytes,
        )
    } == 0
        || info.is_null()
        || bytes < std::mem::size_of::<FixedInfo>() as u32
    {
        return Err(bundle::untrusted("浏览器未提供有效版本资源"));
    }
    let start = info as usize;
    let base = data.as_ptr() as usize;
    if start < base
        || start
            .checked_add(bytes as usize)
            .is_none_or(|end| end > base + data.len())
    {
        return Err(bundle::untrusted("浏览器版本资源越界"));
    }
    let value = unsafe { std::ptr::read_unaligned(info.cast::<FixedInfo>()) };
    if value.signature != 0xfeef04bd {
        return Err(bundle::untrusted("浏览器版本资源签名无效"));
    }
    Ok([
        value.file_ms >> 16,
        value.file_ms & 0xffff,
        value.file_ls >> 16,
        value.file_ls & 0xffff,
    ])
}
