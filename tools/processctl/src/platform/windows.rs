use std::ffi::OsString;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus};

use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenThread, QueryFullProcessImageNameW, ResumeThread,
    CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
};

use crate::process::{ProcessError, ProcessIdentity, SpawnSpec, StartMarker};

pub(crate) struct PlatformChild {
    child: Child,
    job: OwnedHandle,
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

pub(crate) fn spawn(spec: &SpawnSpec) -> Result<(PlatformChild, ProcessIdentity), ProcessError> {
    let job = create_kill_on_close_job().map_err(|source| ProcessError::Io {
        operation: "create kill-on-close job",
        source,
    })?;
    let mut command = Command::new(&spec.executable);
    command
        .args(&spec.args)
        .env_clear()
        .envs(&spec.env)
        .current_dir(&spec.cwd)
        .stdin(std::process::Stdio::null())
        .stdout(spec.stdout.open()?)
        .stderr(spec.stderr.open()?)
        .creation_flags(CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP);

    let mut child = command.spawn().map_err(|source| ProcessError::Io {
        operation: "spawn suspended process",
        source,
    })?;
    let process = child.as_raw_handle() as HANDLE;
    if unsafe { AssignProcessToJobObject(job.0, process) } == 0 {
        let source = std::io::Error::last_os_error();
        let _ = child.kill();
        let _ = child.wait();
        return Err(ProcessError::Io {
            operation: "assign process to job before resume",
            source,
        });
    }

    let identity = match observe_process(process, child.id()) {
        Ok(identity) => identity,
        Err(source) => {
            let _ = unsafe { TerminateJobObject(job.0, 1) };
            let _ = child.wait();
            return Err(ProcessError::Io {
                operation: "read suspended process identity",
                source,
            });
        }
    };
    if let Err(source) = resume_primary_thread(child.id()) {
        let _ = unsafe { TerminateJobObject(job.0, 1) };
        let _ = child.wait();
        return Err(ProcessError::Io {
            operation: "resume assigned process",
            source,
        });
    }

    Ok((PlatformChild { child, job }, identity))
}

impl PlatformChild {
    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub(crate) fn observe_identity(&self) -> std::io::Result<ProcessIdentity> {
        observe_process(self.child.as_raw_handle() as HANDLE, self.child.id())
    }

    pub(crate) fn graceful(&mut self) -> std::io::Result<()> {
        if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, self.child.id()) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub(crate) fn force(&mut self) -> std::io::Result<()> {
        if unsafe { TerminateJobObject(self.job.0, 1) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

fn create_kill_on_close_job() -> std::io::Result<OwnedHandle> {
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let job = OwnedHandle(handle);
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let ok = unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&raw const info).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(job)
}

fn resume_primary_thread(pid: u32) -> std::io::Result<()> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let snapshot = OwnedHandle(snapshot);
    let mut entry: THREADENTRY32 = unsafe { zeroed() };
    entry.dwSize = size_of::<THREADENTRY32>() as u32;
    let mut has_entry = unsafe { Thread32First(snapshot.0, &mut entry) } != 0;
    while has_entry {
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let thread = OwnedHandle(thread);
            let previous = unsafe { ResumeThread(thread.0) };
            if previous == u32::MAX {
                return Err(std::io::Error::last_os_error());
            }
            return Ok(());
        }
        has_entry = unsafe { Thread32Next(snapshot.0, &mut entry) } != 0;
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "primary thread not present in suspended process",
    ))
}

fn observe_process(process: HANDLE, pid: u32) -> std::io::Result<ProcessIdentity> {
    let mut path = vec![0u16; 32_768];
    let mut len = path.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    path.truncate(len as usize);

    let mut created: FILETIME = unsafe { zeroed() };
    let mut exited: FILETIME = unsafe { zeroed() };
    let mut kernel: FILETIME = unsafe { zeroed() };
    let mut user: FILETIME = unsafe { zeroed() };
    if unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let marker = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    Ok(ProcessIdentity {
        pid,
        executable: PathBuf::from(OsString::from_wide(&path)),
        started: StartMarker(marker),
    })
}
