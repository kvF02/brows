//! Windows-only private CDP transport. Only two explicit pipe handles are
//! inherited. The job closes the entire process tree on controller failure.
#![allow(unsafe_code)]
use std::{
    ffi::{c_void, OsString},
    fs::File,
    io, mem,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr,
};
type Handle = *mut c_void;
#[repr(C)]
struct Startup {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_chars: u32,
    y_chars: u32,
    fill: u32,
    flags: u32,
    show: u16,
    reserved_size: u16,
    reserved2: *mut u8,
    input: Handle,
    output: Handle,
    error: Handle,
}
#[repr(C)]
struct StartupEx {
    base: Startup,
    attributes: Handle,
}
#[repr(C)]
struct ProcessInfo {
    process: Handle,
    thread: Handle,
    id: u32,
    thread_id: u32,
}
#[repr(C)]
#[derive(Default)]
struct BasicLimits {
    process_time: i64,
    job_time: i64,
    flags: u32,
    min_working: usize,
    max_working: usize,
    active: u32,
    affinity: usize,
    priority: u32,
    scheduling: u32,
}
#[repr(C)]
#[derive(Default)]
struct IoCounters {
    values: [u64; 6],
}
#[repr(C)]
#[derive(Default)]
struct ExtendedLimits {
    basic: BasicLimits,
    io: IoCounters,
    process_memory: usize,
    job_memory: usize,
    peak_process: usize,
    peak_job: usize,
}
#[link(name = "kernel32")]
extern "system" {
    fn CreatePipe(read: *mut Handle, write: *mut Handle, security: Handle, size: u32) -> i32;
    fn SetHandleInformation(handle: Handle, mask: u32, flags: u32) -> i32;
    fn InitializeProcThreadAttributeList(
        list: Handle,
        count: u32,
        flags: u32,
        size: *mut usize,
    ) -> i32;
    fn UpdateProcThreadAttribute(
        list: Handle,
        flags: u32,
        attribute: usize,
        value: Handle,
        size: usize,
        previous: Handle,
        returned: *mut usize,
    ) -> i32;
    fn DeleteProcThreadAttributeList(list: Handle);
    fn CreateProcessW(
        application: *const u16,
        command: *mut u16,
        process_security: Handle,
        thread_security: Handle,
        inherit: i32,
        flags: u32,
        environment: Handle,
        directory: *const u16,
        startup: *mut Startup,
        process: *mut ProcessInfo,
    ) -> i32;
    fn CreateJobObjectW(attributes: Handle, name: *const u16) -> Handle;
    fn SetInformationJobObject(job: Handle, class: u32, information: Handle, size: u32) -> i32;
    fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
    fn TerminateJobObject(job: Handle, code: u32) -> i32;
    fn ResumeThread(thread: Handle) -> u32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
    fn TerminateProcess(process: Handle, code: u32) -> i32;
}
#[derive(Debug)]
pub(super) struct Process {
    process: OwnedHandle,
    job: OwnedHandle,
    id: u32,
}
impl Process {
    pub(super) fn id(&self) -> u32 {
        self.id
    }
    pub(super) fn kill(&self) -> io::Result<()> {
        if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    pub(super) fn try_wait(&self) -> io::Result<Option<u32>> {
        match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
            258 => Ok(None),
            0 => {
                let mut code = 0;
                if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(Some(code))
                }
            }
            _ => Err(io::Error::last_os_error()),
        }
    }
    pub(super) fn wait(&self) -> io::Result<()> {
        if unsafe { WaitForSingleObject(self.process.as_raw_handle(), 10000) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.kill();
        let _ = self.wait();
    }
}
fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (mut read, mut write) = (ptr::null_mut(), ptr::null_mut());
    if unsafe { CreatePipe(&mut read, &mut write, ptr::null_mut(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreatePipe returned two distinct owned handles.
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    })
}
fn quote(value: &std::ffi::OsStr) -> Vec<u16> {
    let mut out = vec![34];
    let mut slashes = 0;
    for c in value.encode_wide() {
        if c == 92 {
            slashes += 1;
            continue;
        }
        if c == 34 {
            out.extend(std::iter::repeat_n(92, slashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n(92, slashes));
        }
        slashes = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat_n(92, slashes * 2));
    out.push(34);
    out
}
pub(super) fn spawn(
    executable: &Path,
    arguments: &[OsString],
) -> io::Result<(Process, File, File)> {
    let _guard = super::LAUNCH_LOCK
        .lock()
        .map_err(|_| io::Error::other("launch lock poisoned"))?;
    let (child_read, parent_write) = pipe()?;
    let (parent_read, child_write) = pipe()?;
    for handle in [&child_read, &child_write] {
        if unsafe { SetHandleInformation(handle.as_raw_handle(), 1, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let mut size = 0;
    unsafe {
        InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size);
    }
    if size == 0 || size > 65536 {
        return Err(io::Error::other("invalid handle attribute size"));
    }
    let mut storage = vec![0usize; size.div_ceil(mem::size_of::<usize>())];
    let list = storage.as_mut_ptr().cast();
    if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) } == 0 {
        return Err(io::Error::last_os_error());
    }
    struct Attributes(Handle);
    impl Drop for Attributes {
        fn drop(&mut self) {
            unsafe {
                DeleteProcThreadAttributeList(self.0);
            }
        }
    }
    let attributes = Attributes(list);
    let mut handles = [child_read.as_raw_handle(), child_write.as_raw_handle()];
    if unsafe {
        UpdateProcThreadAttribute(
            attributes.0,
            0,
            0x20002,
            handles.as_mut_ptr().cast(),
            mem::size_of_val(&handles),
            ptr::null_mut(),
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let extra = OsString::from(format!(
        "--remote-debugging-io-pipes={},{}",
        handles[0] as usize, handles[1] as usize
    ));
    let mut command = quote(executable.as_os_str());
    for arg in arguments
        .iter()
        .chain([OsString::from("--remote-debugging-pipe"), extra].iter())
    {
        command.push(32);
        command.extend(quote(arg));
    }
    if command.len() > 32765 {
        return Err(io::Error::other("browser command line too large"));
    }
    command.push(0);
    let application: Vec<u16> = executable.as_os_str().encode_wide().chain([0]).collect();
    let mut startup: StartupEx = unsafe { mem::zeroed() };
    startup.base.cb = mem::size_of::<StartupEx>() as u32;
    startup.attributes = attributes.0;
    let mut info: ProcessInfo = unsafe { mem::zeroed() };
    let job = unsafe { CreateJobObjectW(ptr::null_mut(), ptr::null()) };
    if job.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = unsafe { OwnedHandle::from_raw_handle(job) };
    let mut limits = ExtendedLimits::default();
    limits.basic.flags = 0x2000;
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            9,
            (&mut limits as *mut ExtendedLimits).cast(),
            mem::size_of::<ExtendedLimits>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if unsafe {
        CreateProcessW(
            application.as_ptr(),
            command.as_mut_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            1,
            0x80004,
            ptr::null_mut(),
            ptr::null(),
            &mut startup.base,
            &mut info,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let process = unsafe { OwnedHandle::from_raw_handle(info.process) };
    let thread = unsafe { OwnedHandle::from_raw_handle(info.thread) };
    if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()) } == 0 {
        unsafe {
            TerminateProcess(process.as_raw_handle(), 1);
            WaitForSingleObject(process.as_raw_handle(), 10000);
        }
        return Err(io::Error::last_os_error());
    }
    let process = Process {
        process,
        job,
        id: info.id,
    };
    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    drop(child_read);
    drop(child_write);
    Ok((process, File::from(parent_write), File::from(parent_read)))
}
