use std::{
    io,
    process::{Child, Command, ExitStatus},
};

/// Windows assigns the suspended browser to a kill-on-close job before any of
/// its code can spawn children. A manager crash therefore closes the entire tree.
#[derive(Debug)]
pub(crate) struct BrowserChild {
    child: Child,
    #[cfg(windows)]
    job: windows::BrowserJob,
}

impl BrowserChild {
    pub(crate) fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(windows)]
        let job = {
            use std::os::windows::process::CommandExt;
            command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
            windows::BrowserJob::new()?
        };
        #[allow(unused_mut)]
        let mut child = command.spawn()?;
        #[cfg(windows)]
        if let Err(error) = job.attach_and_resume(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(Self {
            child,
            #[cfg(windows)]
            job,
        })
    }

    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
    pub(crate) fn stop(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        self.job.terminate()?;
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
        }
        self.child.wait()?;
        Ok(())
    }
}

impl Drop for BrowserChild {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows {
    use std::{
        io,
        mem::{size_of, zeroed},
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::Child,
    };
    use windows_sys::Win32::{
        Foundation::{HANDLE, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD,
                THREADENTRY32,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
        },
    };

    #[derive(Debug)]
    pub(super) struct BrowserJob(OwnedHandle);

    impl BrowserJob {
        pub(super) fn new() -> io::Result<Self> {
            // SAFETY: null security attributes make the handle non-inheritable;
            // every successful raw handle is immediately owned and closed by RAII.
            unsafe {
                let raw = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if raw.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let job = Self(OwnedHandle::from_raw_handle(raw));
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    raw,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) == 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(job)
            }
        }

        pub(super) fn attach_and_resume(&self, child: &Child) -> io::Result<()> {
            // SAFETY: both handles stay alive for each API call. The process was
            // created suspended, so it cannot create descendants before assignment.
            unsafe {
                if AssignProcessToJobObject(
                    self.0.as_raw_handle() as HANDLE,
                    child.as_raw_handle() as HANDLE,
                ) == 0
                {
                    return Err(io::Error::last_os_error());
                }
                let raw = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
                if raw == INVALID_HANDLE_VALUE {
                    return Err(io::Error::last_os_error());
                }
                let snapshot = OwnedHandle::from_raw_handle(raw);
                let mut entry: THREADENTRY32 = zeroed();
                entry.dwSize = size_of::<THREADENTRY32>() as u32;
                let mut found = Thread32First(snapshot.as_raw_handle() as HANDLE, &mut entry);
                while found != 0 {
                    if entry.th32OwnerProcessID == child.id() {
                        let raw = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                        if raw.is_null() {
                            return Err(io::Error::last_os_error());
                        }
                        let thread = OwnedHandle::from_raw_handle(raw);
                        if ResumeThread(thread.as_raw_handle() as HANDLE) == u32::MAX {
                            return Err(io::Error::last_os_error());
                        }
                        return Ok(());
                    }
                    found = Thread32Next(snapshot.as_raw_handle() as HANDLE, &mut entry);
                }
                Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "suspended browser thread was not found",
                ))
            }
        }

        pub(super) fn terminate(&self) -> io::Result<()> {
            // SAFETY: the job handle is owned and valid for the duration of the call.
            if unsafe { TerminateJobObject(self.0.as_raw_handle() as HANDLE, 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }
}

#[cfg(all(test, windows))]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use std::{
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        time::{Duration, Instant},
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    // Subprocess-only helpers are launched by the test below, never by ordinary runs.
    #[test]
    #[ignore = "subprocess helper"]
    fn tree_child() {
        let Some(marker) = std::env::var_os("BROWS_TREE_TEST_MARKER") else {
            return;
        };
        let mut grandchild = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "process::tests::tree_grandchild"])
            .spawn()
            .unwrap();
        std::fs::write(marker, grandchild.id().to_string()).unwrap();
        let _ = grandchild.wait();
    }

    #[test]
    #[ignore = "subprocess helper"]
    fn tree_grandchild() {
        if std::env::var_os("BROWS_TREE_TEST_MARKER").is_some() {
            std::thread::sleep(Duration::from_secs(30));
        }
    }

    #[test]
    fn stopping_or_dropping_a_browser_terminates_its_descendants() {
        for explicit in [true, false] {
            let marker = std::env::temp_dir().join(format!(
                "brows-tree-{}-{explicit}-{}.pid",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let mut child = BrowserChild::spawn(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--ignored", "--exact", "process::tests::tree_child"])
                    .env("BROWS_TREE_TEST_MARKER", &marker),
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !marker.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            let pid: u32 = std::fs::read_to_string(&marker)
                .expect("descendant started")
                .parse()
                .unwrap();
            // SAFETY: only the PID created by this test is opened. The handle is
            // held across termination to avoid PID-reuse ambiguity and is closed by RAII.
            let process = unsafe {
                let raw = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
                assert!(!raw.is_null());
                OwnedHandle::from_raw_handle(raw)
            };
            if explicit {
                child.stop().unwrap();
            }
            drop(child);
            assert_eq!(
                unsafe { WaitForSingleObject(process.as_raw_handle(), 5_000) },
                0
            );
            std::fs::remove_file(marker).unwrap();
        }
    }
}
