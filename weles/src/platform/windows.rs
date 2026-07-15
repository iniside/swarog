//! Windows containment: raw `CreateProcessW` (suspended, own console process
//! group, explicit sorted UTF-16 environment block) assigned to a
//! kill-on-close Job Object BEFORE its first instruction runs, then resumed.
//! Graceful = `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid)` (valid because
//! `CREATE_NEW_PROCESS_GROUP` makes the pid the group id); force =
//! `TerminateJobObject`. Technique copied from
//! `tools/processctl/src/platform/windows.rs`, simplified: no guardian frames,
//! no identity marker, no handle allow-list (plain inheritable std handles).

use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, GetCurrentProcess, GetExitCodeProcess, ResumeThread, TerminateProcess,
    WaitForSingleObject, CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
};

use super::{ExitInfo, SpawnSpec};

pub(super) struct PlatformProc {
    // Field order matters: Rust drops fields in declaration order, so the
    // process handle closes first and the job handle closes LAST — closing a
    // KILL_ON_JOB_CLOSE job is itself the backstop that kills any survivor.
    process: OwnedHandle,
    job: OwnedHandle,
    pid: u32,
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: this struct uniquely owns a valid handle; closing once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

pub(super) fn spawn(spec: SpawnSpec) -> Result<PlatformProc> {
    let job = create_kill_on_close_job().context("create kill-on-close job")?;
    let stdin = open_null_inheritable().context("open child stdin (NUL)")?;
    let stdout = child_output(spec.stdout.as_ref()).context("prepare child stdout")?;
    let stderr = child_output(spec.stderr.as_ref()).context("prepare child stderr")?;

    // SAFETY: STARTUPINFOW is a plain-old-data struct; all-zero is a valid
    // initial state before setting cb and the std-handle fields.
    let mut startup: STARTUPINFOW = unsafe { zeroed() };
    startup.cb = size_of::<STARTUPINFOW>() as u32;
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = stdin.0;
    startup.hStdOutput = stdout.0;
    startup.hStdError = stderr.0;

    let mut command_line = command_line(&spec.program, &spec.args)?;
    let application = wide_nul(spec.program.as_os_str(), "program path")?;
    let cwd = match &spec.cwd {
        Some(cwd) => Some(wide_nul(cwd.as_os_str(), "working directory")?),
        None => None,
    };
    let environment = environment_block(&spec.env)?;
    // SAFETY: PROCESS_INFORMATION is plain-old-data output storage.
    let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
    // SAFETY: application/command_line/cwd are NUL-terminated UTF-16 buffers
    // alive across the call, environment is a double-NUL-terminated UTF-16
    // block (CREATE_UNICODE_ENVIRONMENT), and the std handles in `startup`
    // are inheritable and alive until after the call returns.
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            cwd.as_ref().map_or(std::ptr::null(), |cwd| cwd.as_ptr()),
            &startup,
            &mut info,
        )
    };
    if created == 0 {
        return Err(std::io::Error::last_os_error()).context("create suspended process");
    }
    let process = OwnedHandle(info.hProcess);
    let thread = OwnedHandle(info.hThread);

    // Assign to the job while still suspended, so the child cannot spawn
    // anything outside the job before containment applies.
    // SAFETY: both handles are valid and owned by this function.
    if unsafe { AssignProcessToJobObject(job.0, process.0) } == 0 {
        let source = std::io::Error::last_os_error();
        terminate_unassigned(&process).context("clean up unassigned suspended process")?;
        return Err(source).context("assign suspended process to job");
    }
    // SAFETY: thread is the valid primary-thread handle from CreateProcessW.
    if unsafe { ResumeThread(thread.0) } == u32::MAX {
        let source = std::io::Error::last_os_error();
        // SAFETY: job is a valid job handle owning the suspended process.
        unsafe { TerminateJobObject(job.0, 1) };
        return Err(source).context("resume assigned primary thread");
    }
    drop(thread);

    Ok(PlatformProc {
        process,
        job,
        pid: info.dwProcessId,
    })
}

impl PlatformProc {
    pub(super) fn pid(&self) -> u32 {
        self.pid
    }

    pub(super) fn try_wait(&mut self) -> std::io::Result<Option<ExitInfo>> {
        // SAFETY: process is a valid handle with SYNCHRONIZE access owned by us.
        match unsafe { WaitForSingleObject(self.process.0, 0) } {
            WAIT_OBJECT_0 => {
                let mut code = 0u32;
                // SAFETY: process is a valid handle; code is out storage.
                if unsafe { GetExitCodeProcess(self.process.0, &mut code) } == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(Some(ExitInfo {
                    code: Some(code as i32),
                }))
            }
            WAIT_TIMEOUT => Ok(None),
            _ => Err(std::io::Error::last_os_error()),
        }
    }

    pub(super) fn graceful(&mut self) -> std::io::Result<()> {
        // SAFETY: pid was created with CREATE_NEW_PROCESS_GROUP, so it names a
        // console process group we own; no handle is involved.
        if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, self.pid) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub(super) fn force(&mut self) -> std::io::Result<()> {
        // SAFETY: job is our valid kill-on-close job handle; terminating it
        // kills every process assigned to it (the whole tree by parentage).
        if unsafe { TerminateJobObject(self.job.0, 1) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

fn create_kill_on_close_job() -> Result<OwnedHandle> {
    // SAFETY: null attributes/name request a fresh anonymous job object.
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error()).context("CreateJobObjectW");
    }
    let job = OwnedHandle(handle);
    // SAFETY: JOBOBJECT_EXTENDED_LIMIT_INFORMATION is plain-old-data.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: job is valid; info matches the class and declared size.
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&raw const info).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("SetInformationJobObject");
    }
    Ok(job)
}

fn terminate_unassigned(process: &OwnedHandle) -> Result<()> {
    // SAFETY: process is a valid handle to the suspended child we just made.
    if unsafe { TerminateProcess(process.0, 1) } == 0 {
        return Err(std::io::Error::last_os_error()).context("TerminateProcess");
    }
    // SAFETY: bounded wait on our own valid process handle.
    match unsafe { WaitForSingleObject(process.0, 5_000) } {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => bail!("timed out reaping unassigned suspended process"),
        _ => Err(std::io::Error::last_os_error()).context("wait for terminated process"),
    }
}

/// Duplicates the caller-provided log file as an inheritable handle, or opens
/// inheritable `NUL` when the spec leaves the stream unset.
fn child_output(file: Option<&std::fs::File>) -> Result<OwnedHandle> {
    match file {
        Some(file) => duplicate_inheritable(file.as_raw_handle() as HANDLE),
        None => open_null_inheritable(),
    }
}

fn open_null_inheritable() -> Result<OwnedHandle> {
    let path = wide_nul(OsStr::new("NUL"), "NUL device path")?;
    // SAFETY: SECURITY_ATTRIBUTES is plain-old-data; we set both fields used.
    let mut security: SECURITY_ATTRIBUTES = unsafe { zeroed() };
    security.nLength = size_of::<SECURITY_ATTRIBUTES>() as u32;
    security.bInheritHandle = 1;
    // SAFETY: path is NUL-terminated UTF-16; security outlives the call.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &security,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        Err(std::io::Error::last_os_error()).context("open NUL device")
    } else {
        Ok(OwnedHandle(handle))
    }
}

fn duplicate_inheritable(source: HANDLE) -> Result<OwnedHandle> {
    if source.is_null() || source == INVALID_HANDLE_VALUE {
        bail!("invalid stdio handle in spawn spec");
    }
    // SAFETY: GetCurrentProcess returns a pseudo-handle; source is a live
    // handle owned by the caller's File for the duration of this call.
    let current = unsafe { GetCurrentProcess() };
    let mut duplicated = std::ptr::null_mut();
    // SAFETY: valid source/target processes; duplicated is out storage; the
    // bInheritHandle=1 argument makes the copy inheritable.
    if unsafe {
        DuplicateHandle(
            current,
            source,
            current,
            &mut duplicated,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        Err(std::io::Error::last_os_error()).context("duplicate stdio handle")
    } else {
        Ok(OwnedHandle(duplicated))
    }
}

/// Builds the full command line with Windows argv quoting rules (backslash
/// runs before quotes doubled, embedded quotes backslash-escaped).
fn command_line(program: &Path, args: &[OsString]) -> Result<Vec<u16>> {
    let mut command = Vec::new();
    append_quoted(&mut command, program.as_os_str())?;
    for arg in args {
        command.push(b' ' as u16);
        append_quoted(&mut command, arg)?;
    }
    command.push(0);
    Ok(command)
}

fn append_quoted(command: &mut Vec<u16>, argument: &OsStr) -> Result<()> {
    let wide: Vec<u16> = argument.encode_wide().collect();
    if wide.contains(&0) {
        bail!("process argument contains NUL");
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

/// Builds the CreateProcessW environment block: `KEY=VALUE\0` entries sorted
/// case-insensitively by key, terminated by an extra NUL (two NULs when empty).
fn environment_block(environment: &std::collections::BTreeMap<OsString, OsString>) -> Result<Vec<u16>> {
    let mut entries = Vec::with_capacity(environment.len());
    for (key, value) in environment {
        let key: Vec<u16> = key.encode_wide().collect();
        let value: Vec<u16> = value.encode_wide().collect();
        if key.is_empty() || key.contains(&(b'=' as u16)) || key.contains(&0) || value.contains(&0)
        {
            bail!("invalid process environment entry");
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

fn wide_nul(value: &OsStr, what: &'static str) -> Result<Vec<u16>> {
    let mut wide: Vec<u16> = value.encode_wide().collect();
    if wide.contains(&0) {
        return Err(anyhow!("{what} contains NUL"));
    }
    wide.push(0);
    Ok(wide)
}
