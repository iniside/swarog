use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, FILETIME, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    GenerateConsoleCtrlEvent, GetStdHandle, CTRL_BREAK_EVENT, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    GetProcessTimes, InitializeProcThreadAttributeList, QueryFullProcessImageNameW, ResumeThread,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject, CREATE_NEW_PROCESS_GROUP,
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
    PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use crate::process::{
    OutputDestination, ProcessError, ProcessGroupPolicy, ProcessIdentity, SpawnSpec, StartMarker,
};

pub(crate) struct PlatformChild {
    process: OwnedHandle,
    job: OwnedHandle,
    pid: u32,
    root_status: Option<ExitStatus>,
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

struct AttributeList {
    storage: Vec<u8>,
}

impl AttributeList {
    fn new(handles: &mut [HANDLE]) -> std::io::Result<Self> {
        let mut size = 0usize;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        }
        if size == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut storage = vec![0u8; size];
        let list = storage.as_mut_ptr().cast();
        if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let updated = unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                std::mem::size_of_val(handles),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if updated == 0 {
            unsafe { DeleteProcThreadAttributeList(list) };
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { storage })
    }

    fn as_mut_ptr(&mut self) -> *mut std::ffi::c_void {
        self.storage.as_mut_ptr().cast()
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
    }
}

pub(crate) fn spawn(spec: &SpawnSpec) -> Result<(PlatformChild, ProcessIdentity), ProcessError> {
    match spec.process_group {
        ProcessGroupPolicy::Owned => {}
    }
    let executable =
        std::fs::canonicalize(&spec.executable).map_err(|source| ProcessError::Io {
            operation: "canonicalize process executable",
            source,
        })?;
    let job = create_kill_on_close_job().map_err(|source| ProcessError::Io {
        operation: "create kill-on-close job",
        source,
    })?;
    let stdin = open_null(true).map_err(|source| ProcessError::Io {
        operation: "open child stdin",
        source,
    })?;
    let stdout =
        open_output(&spec.stdout, STD_OUTPUT_HANDLE).map_err(|source| ProcessError::Io {
            operation: "open child stdout",
            source,
        })?;
    let stderr =
        open_output(&spec.stderr, STD_ERROR_HANDLE).map_err(|source| ProcessError::Io {
            operation: "open child stderr",
            source,
        })?;
    let mut inherited = [stdin.0, stdout.0, stderr.0];
    let mut attributes = AttributeList::new(&mut inherited).map_err(|source| ProcessError::Io {
        operation: "create child handle allow-list",
        source,
    })?;

    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin.0;
    startup.StartupInfo.hStdOutput = stdout.0;
    startup.StartupInfo.hStdError = stderr.0;
    startup.lpAttributeList = attributes.as_mut_ptr();

    let mut command_line = command_line(&executable, &spec.args)?;
    let application = wide_nul(executable.as_os_str(), "executable path")?;
    let cwd = wide_nul(spec.cwd.as_os_str(), "working directory")?;
    let environment = environment_block(&spec.env)?;
    let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_SUSPENDED
                | CREATE_NEW_PROCESS_GROUP
                | CREATE_UNICODE_ENVIRONMENT
                | EXTENDED_STARTUPINFO_PRESENT,
            environment.as_ptr().cast(),
            cwd.as_ptr(),
            (&raw const startup.StartupInfo),
            &mut info,
        )
    };
    if created == 0 {
        return Err(ProcessError::Io {
            operation: "create suspended process",
            source: std::io::Error::last_os_error(),
        });
    }
    let process = OwnedHandle(info.hProcess);
    let thread = OwnedHandle(info.hThread);

    if unsafe { AssignProcessToJobObject(job.0, process.0) } == 0 {
        let source = std::io::Error::last_os_error();
        if let Err(cleanup) = terminate_unassigned(&process) {
            return Err(ProcessError::Io {
                operation: "clean up unassigned suspended process",
                source: cleanup,
            });
        }
        return Err(ProcessError::Io {
            operation: "assign suspended process to job",
            source,
        });
    }
    let identity = match observe_process(process.0, info.dwProcessId) {
        Ok(identity) => identity,
        Err(source) => {
            if let Err(cleanup) = terminate_job_and_wait(&job) {
                return Err(ProcessError::Io {
                    operation: "clean up job after identity failure",
                    source: cleanup,
                });
            }
            return Err(ProcessError::Io {
                operation: "read suspended process identity",
                source,
            });
        }
    };
    if unsafe { ResumeThread(thread.0) } == u32::MAX {
        let source = std::io::Error::last_os_error();
        if let Err(cleanup) = terminate_job_and_wait(&job) {
            return Err(ProcessError::Io {
                operation: "clean up job after resume failure",
                source: cleanup,
            });
        }
        return Err(ProcessError::Io {
            operation: "resume assigned primary thread",
            source,
        });
    }
    drop(thread);

    Ok((
        PlatformChild {
            process,
            job,
            pid: info.dwProcessId,
            root_status: None,
        },
        identity,
    ))
}

impl PlatformChild {
    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        if self.root_status.is_none() {
            match unsafe { WaitForSingleObject(self.process.0, 0) } {
                WAIT_OBJECT_0 => self.root_status = Some(exit_status(self.process.0)?),
                WAIT_TIMEOUT => {}
                _ => return Err(std::io::Error::last_os_error()),
            }
        }
        if job_active_processes(self.job.0)? == 0 {
            if self.root_status.is_none() {
                self.root_status = Some(exit_status(self.process.0)?);
            }
            Ok(self.root_status)
        } else {
            Ok(None)
        }
    }

    pub(crate) fn graceful(&mut self) -> std::io::Result<()> {
        if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, self.pid) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub(crate) fn completion_forced_remainder(&self, _status: ExitStatus) -> bool {
        false
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
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&raw const info).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(job)
}

fn job_active_processes(job: HANDLE) -> std::io::Result<u32> {
    let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
    if unsafe {
        QueryInformationJobObject(
            job,
            JobObjectBasicAccountingInformation,
            (&raw mut info).cast(),
            size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(info.ActiveProcesses)
    }
}

fn exit_status(process: HANDLE) -> std::io::Result<ExitStatus> {
    let mut code = 0u32;
    if unsafe { GetExitCodeProcess(process, &mut code) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(ExitStatus::from_raw(code))
    }
}

fn terminate_unassigned(process: &OwnedHandle) -> std::io::Result<()> {
    if unsafe { TerminateProcess(process.0, 1) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    match unsafe { WaitForSingleObject(process.0, 5_000) } {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out reaping unassigned suspended process",
        )),
        _ => Err(std::io::Error::last_os_error()),
    }
}

fn terminate_job_and_wait(job: &OwnedHandle) -> std::io::Result<()> {
    if unsafe { TerminateJobObject(job.0, 1) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if job_active_processes(job.0)? == 0 {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out draining failed process job",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn open_output(destination: &OutputDestination, inherited: u32) -> std::io::Result<OwnedHandle> {
    match destination {
        OutputDestination::Inherit => duplicate_inheritable(unsafe { GetStdHandle(inherited) }),
        OutputDestination::Null => open_null(false),
        OutputDestination::File(path) => create_inheritable_file(path, false),
    }
}

fn open_null(read: bool) -> std::io::Result<OwnedHandle> {
    create_inheritable_file(Path::new("NUL"), read)
}

fn create_inheritable_file(path: &Path, read: bool) -> std::io::Result<OwnedHandle> {
    let mut path: Vec<u16> = path.as_os_str().encode_wide().collect();
    if path.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "stdio path contains NUL",
        ));
    }
    path.push(0);
    let mut security: SECURITY_ATTRIBUTES = unsafe { zeroed() };
    security.nLength = size_of::<SECURITY_ATTRIBUTES>() as u32;
    security.bInheritHandle = 1;
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            if read { GENERIC_READ } else { GENERIC_WRITE },
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &security,
            if read { OPEN_EXISTING } else { CREATE_ALWAYS },
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(OwnedHandle(handle))
    }
}

fn duplicate_inheritable(source: HANDLE) -> std::io::Result<OwnedHandle> {
    if source.is_null() || source == INVALID_HANDLE_VALUE {
        return open_null(false);
    }
    let current = unsafe { GetCurrentProcess() };
    let mut duplicated = std::ptr::null_mut();
    if unsafe {
        DuplicateHandle(
            current,
            source,
            current,
            &mut duplicated,
            0,
            1,
            windows_sys::Win32::Foundation::DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(OwnedHandle(duplicated))
    }
}

pub(super) fn command_line(executable: &Path, args: &[OsString]) -> Result<Vec<u16>, ProcessError> {
    let mut command = Vec::new();
    append_quoted(&mut command, executable.as_os_str())?;
    for arg in args {
        command.push(b' ' as u16);
        append_quoted(&mut command, arg)?;
    }
    command.push(0);
    Ok(command)
}

fn append_quoted(command: &mut Vec<u16>, argument: &OsStr) -> Result<(), ProcessError> {
    let wide: Vec<u16> = argument.encode_wide().collect();
    if wide.contains(&0) {
        return Err(invalid_spec("process argument contains NUL"));
    }
    let needs_quotes = wide.is_empty()
        || wide
            .iter()
            .any(|unit| *unit == b' ' as u16 || *unit == b'\t' as u16 || *unit == b'"' as u16);
    if !needs_quotes {
        command.extend_from_slice(&wide);
        return Ok(());
    }
    command.push(b'"' as u16);
    let mut slashes = 0usize;
    for unit in wide {
        if unit == b'\\' as u16 {
            slashes += 1;
        } else if unit == b'"' as u16 {
            command.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2 + 1));
            command.push(unit);
            slashes = 0;
        } else {
            command.extend(std::iter::repeat_n(b'\\' as u16, slashes));
            slashes = 0;
            command.push(unit);
        }
    }
    command.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    command.push(b'"' as u16);
    Ok(())
}

pub(super) fn environment_block(
    environment: &std::collections::BTreeMap<OsString, OsString>,
) -> Result<Vec<u16>, ProcessError> {
    let mut entries = Vec::with_capacity(environment.len());
    for (key, value) in environment {
        let key: Vec<u16> = key.encode_wide().collect();
        let value: Vec<u16> = value.encode_wide().collect();
        if key.is_empty() || key.contains(&(b'=' as u16)) || key.contains(&0) || value.contains(&0)
        {
            return Err(invalid_spec("invalid process environment entry"));
        }
        entries.push((key, value));
    }
    entries.sort_by(|(left, _), (right, _)| compare_environment_keys(left, right));
    let mut block = Vec::new();
    for (key, value) in entries {
        block.extend(key);
        block.push(b'=' as u16);
        block.extend(value);
        block.push(0);
    }
    block.push(0);
    if environment.is_empty() {
        block.push(0);
    }
    Ok(block)
}

fn compare_environment_keys(left: &[u16], right: &[u16]) -> Ordering {
    OsString::from_wide(left)
        .to_string_lossy()
        .to_uppercase()
        .cmp(&OsString::from_wide(right).to_string_lossy().to_uppercase())
}

fn wide_nul(value: &OsStr, what: &'static str) -> Result<Vec<u16>, ProcessError> {
    let mut wide: Vec<u16> = value.encode_wide().collect();
    if wide.contains(&0) {
        return Err(invalid_spec(what));
    }
    wide.push(0);
    Ok(wide)
}

fn invalid_spec(message: &'static str) -> ProcessError {
    ProcessError::Io {
        operation: "validate process specification",
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, message),
    }
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
