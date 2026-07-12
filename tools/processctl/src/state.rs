use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{fs::File, io::Read};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{OwnedChild, ProcessIdentity, ShutdownPolicy};

pub const STATE_VERSION: u32 = 1;
pub const MAX_STATE_BYTES: u64 = 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Atomically writes arbitrary owner-only tooling data using the same platform
/// permissions and replacement path as the private fleet state.
pub fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), StateError> {
    let store = StateStore::new(path);
    let parent = path.parent().ok_or(StateError::InvalidField {
        field: "private path",
        reason: "must have a parent directory",
    })?;
    let file_name = path.file_name().ok_or(StateError::InvalidField {
        field: "private path",
        reason: "must have a file name",
    })?;
    let temp = parent.join(format!(
        ".{}.{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    store.write_platform(&temp, parent, bytes)
}

/// Validates that a tooling file is a private regular file owned by this user.
pub fn validate_private_path(path: &Path) -> Result<(), StateError> {
    open_state_for_read(path)
        .map(drop)
        .map_err(|source| StateError::Io {
            operation: "validate private tooling file",
            source,
        })
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetStatus {
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "state")]
pub enum ManagedStatus {
    Starting,
    Healthy,
    Stopping,
    Exited { code: Option<i32> },
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedProcess {
    label: String,
    identity: ProcessIdentity,
    status: ManagedStatus,
    stdout_log: PathBuf,
    stderr_log: PathBuf,
}

impl ManagedProcess {
    pub fn new(
        label: impl Into<String>,
        identity: ProcessIdentity,
        stdout_log: PathBuf,
        stderr_log: PathBuf,
    ) -> Result<Self, StateError> {
        let label = label.into();
        validate_identifier("process label", &label)?;
        Ok(Self {
            label,
            identity,
            status: ManagedStatus::Starting,
            stdout_log,
            stderr_log,
        })
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn identity(&self) -> &ProcessIdentity {
        &self.identity
    }

    pub fn status(&self) -> &ManagedStatus {
        &self.status
    }

    pub fn set_status(&mut self, status: ManagedStatus) {
        self.status = status;
    }

    pub fn stdout_log(&self) -> &Path {
        &self.stdout_log
    }

    pub fn stderr_log(&self) -> &Path {
        &self.stderr_log
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FleetState {
    version: u32,
    run_id: String,
    topology: String,
    status: FleetStatus,
    #[serde(default)]
    supervisor: Option<ProcessIdentity>,
    control_endpoint: Option<PathBuf>,
    processes: Vec<ManagedProcess>,
    #[serde(default)]
    failure: Option<FailureRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FailureRecord {
    stage: String,
    process: Option<String>,
}

impl FailureRecord {
    pub fn stage(&self) -> &str {
        &self.stage
    }
    pub fn process(&self) -> Option<&str> {
        self.process.as_deref()
    }
}

impl FleetState {
    pub fn new(run_id: impl Into<String>, topology: impl Into<String>) -> Result<Self, StateError> {
        let run_id = run_id.into();
        let topology = topology.into();
        validate_identifier("run id", &run_id)?;
        validate_identifier("topology", &topology)?;
        Ok(Self {
            version: STATE_VERSION,
            run_id,
            topology,
            status: FleetStatus::Starting,
            supervisor: None,
            control_endpoint: None,
            processes: Vec::new(),
            failure: None,
        })
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn topology(&self) -> &str {
        &self.topology
    }

    pub fn status(&self) -> FleetStatus {
        self.status
    }

    pub fn set_status(&mut self, status: FleetStatus) {
        self.status = status;
    }

    pub fn supervisor(&self) -> Option<&ProcessIdentity> {
        self.supervisor.as_ref()
    }

    pub fn set_supervisor(&mut self, supervisor: ProcessIdentity) {
        self.supervisor = Some(supervisor);
    }

    pub fn control_endpoint(&self) -> Option<&Path> {
        self.control_endpoint.as_deref()
    }

    pub fn set_control_endpoint(&mut self, endpoint: Option<PathBuf>) {
        self.control_endpoint = endpoint;
    }

    pub fn processes(&self) -> &[ManagedProcess] {
        &self.processes
    }

    pub fn processes_mut(&mut self) -> &mut [ManagedProcess] {
        &mut self.processes
    }

    pub fn push_process(&mut self, process: ManagedProcess) {
        self.processes.push(process);
    }

    pub fn failure(&self) -> Option<&FailureRecord> {
        self.failure.as_ref()
    }

    pub fn record_failure(
        &mut self,
        stage: impl Into<String>,
        process: Option<impl Into<String>>,
    ) -> Result<(), StateError> {
        let stage = stage.into();
        validate_identifier("failure stage", &stage)?;
        let process = process.map(Into::into);
        if let Some(process) = &process {
            validate_identifier("failed process", process)?;
        }
        self.failure = Some(FailureRecord { stage, process });
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("invalid {field}: {reason}")]
    InvalidField {
        field: &'static str,
        reason: &'static str,
    },
    #[error("serialize process state: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("{operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("unsupported state version {observed}; expected {expected}")]
    UnsupportedVersion { observed: u32, expected: u32 },
}

#[derive(Debug, Error)]
#[error("state checkpoint failed: {checkpoint}; cleanup failures: {cleanup_failures:?}")]
pub struct StateCheckpointError {
    pub checkpoint: StateError,
    pub cleanup_failures: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct StateStore {
    path: PathBuf,
    #[cfg(test)]
    failure: Option<StateFailurePoint>,
}

impl StateStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            #[cfg(test)]
            failure: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<FleetState>, StateError> {
        let file = match open_state_for_read(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StateError::Io {
                    operation: "read process state",
                    source,
                });
            }
        };
        let mut bytes = Vec::new();
        file.take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| StateError::Io {
                operation: "read process state",
                source,
            })?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(StateError::InvalidField {
                field: "state file",
                reason: "exceeds maximum size",
            });
        }
        let state: FleetState = serde_json::from_slice(&bytes)?;
        if state.version != STATE_VERSION {
            return Err(StateError::UnsupportedVersion {
                observed: state.version,
                expected: STATE_VERSION,
            });
        }
        Ok(Some(state))
    }

    pub fn write_atomic(&self, state: &FleetState) -> Result<(), StateError> {
        if state.version != STATE_VERSION {
            return Err(StateError::UnsupportedVersion {
                observed: state.version,
                expected: STATE_VERSION,
            });
        }
        let bytes = serde_json::to_vec_pretty(state)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(StateError::InvalidField {
                field: "state file",
                reason: "exceeds maximum size",
            });
        }
        let parent = self.path.parent().ok_or(StateError::InvalidField {
            field: "state path",
            reason: "must have a parent directory",
        })?;
        let file_name = self.path.file_name().ok_or(StateError::InvalidField {
            field: "state path",
            reason: "must have a file name",
        })?;
        let temp = parent.join(format!(
            ".{}.{}.{}.tmp",
            file_name.to_string_lossy(),
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        match open_state_for_read(&self.path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(StateError::Io {
                    operation: "validate existing process state",
                    source,
                });
            }
        }
        self.write_platform(&temp, parent, &bytes)
    }

    pub fn checkpoint_or_rollback(
        &self,
        state: &FleetState,
        started: &mut [OwnedChild],
        shutdown: ShutdownPolicy,
    ) -> Result<(), StateCheckpointError> {
        if let Err(checkpoint) = self.write_atomic(state) {
            let mut cleanup_failures = Vec::new();
            for child in started.iter_mut().rev() {
                if let Err(error) = child.shutdown(shutdown) {
                    cleanup_failures.push(error.to_string());
                }
            }
            return Err(StateCheckpointError {
                checkpoint,
                cleanup_failures,
            });
        }
        Ok(())
    }

    fn write_platform(&self, temp: &Path, _parent: &Path, bytes: &[u8]) -> Result<(), StateError> {
        #[cfg(target_os = "linux")]
        {
            self.write_linux(temp, _parent, bytes)
        }
        #[cfg(windows)]
        {
            self.write_windows(temp, bytes)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (temp, _parent, bytes);
            Err(StateError::Io {
                operation: "write process state",
                source: std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "processctl state supports only Windows and Linux",
                ),
            })
        }
    }

    fn inject(&self, point: StateFailurePoint) -> Result<(), StateError> {
        #[cfg(test)]
        if self.failure == Some(point) {
            #[cfg(windows)]
            if point == StateFailurePoint::CrashBeforePublish {
                std::process::abort();
            }
            return Err(StateError::Io {
                operation: "injected state-store failure",
                source: std::io::Error::other(format!("{point:?}")),
            });
        }
        let _ = point;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn write_linux(&self, temp: &Path, parent: &Path, bytes: &[u8]) -> Result<(), StateError> {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        self.inject(StateFailurePoint::CreateTemp)?;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(temp)
            .map_err(|source| StateError::Io {
                operation: "create private state temp file",
                source,
            })?;
        let mut cleanup = TempCleanup::new(temp);
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|source| StateError::Io {
                operation: "secure state temp file",
                source,
            })?;
        validate_private_regular_linux(&file).map_err(|source| StateError::Io {
            operation: "validate private state temp file",
            source,
        })?;
        self.inject(StateFailurePoint::SecureTemp)?;
        self.inject(StateFailurePoint::Write)?;
        file.write_all(bytes).map_err(|source| StateError::Io {
            operation: "write state temp file",
            source,
        })?;
        file.flush().map_err(|source| StateError::Io {
            operation: "flush state temp buffer",
            source,
        })?;
        self.inject(StateFailurePoint::Flush)?;
        file.sync_all().map_err(|source| StateError::Io {
            operation: "sync state temp file",
            source,
        })?;
        drop(file);
        self.inject(StateFailurePoint::Replace)?;
        std::fs::rename(temp, &self.path).map_err(|source| StateError::Io {
            operation: "atomically replace process state",
            source,
        })?;
        cleanup.disarm();
        self.inject(StateFailurePoint::SyncParent)?;
        open_directory_linux(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| StateError::Io {
                operation: "sync state parent directory",
                source,
            })
    }

    #[cfg(windows)]
    fn write_windows(&self, temp: &Path, bytes: &[u8]) -> Result<(), StateError> {
        use std::io::Write;
        use std::os::windows::io::{AsRawHandle, FromRawHandle};

        self.inject(StateFailurePoint::CreateTemp)?;
        self.inject(StateFailurePoint::SecureTemp)?;
        let security = OwnerOnlySecurity::new().map_err(|source| StateError::Io {
            operation: "build owner-only state DACL",
            source,
        })?;
        let handle =
            create_private_file(temp, &security, true).map_err(|source| StateError::Io {
                operation: "create owner-only state temp file",
                source,
            })?;
        let mut cleanup = TempCleanup::new(temp);
        let mut file = unsafe { std::fs::File::from_raw_handle(handle) };
        validate_private_regular_windows(file.as_raw_handle() as _).map_err(|source| {
            StateError::Io {
                operation: "validate owner-only state temp file",
                source,
            }
        })?;
        self.inject(StateFailurePoint::Write)?;
        file.write_all(bytes).map_err(|source| StateError::Io {
            operation: "write state temp file",
            source,
        })?;
        file.flush().map_err(|source| StateError::Io {
            operation: "flush state temp buffer",
            source,
        })?;
        self.inject(StateFailurePoint::Flush)?;
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::FlushFileBuffers(file.as_raw_handle() as _)
        } == 0
        {
            return Err(StateError::Io {
                operation: "flush state file buffers",
                source: std::io::Error::last_os_error(),
            });
        }
        drop(file);

        self.inject(StateFailurePoint::CrashBeforePublish)?;
        self.inject(StateFailurePoint::Replace)?;
        let destination = wide_path(&self.path)?;
        let replacement = wide_path(temp)?;
        let existing = match create_private_file(&self.path, &security, false) {
            Ok(handle) => {
                unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
                true
            }
            Err(error) if matches!(error.raw_os_error(), Some(2 | 3)) => false,
            Err(source) => {
                return Err(StateError::Io {
                    operation: "validate existing state destination",
                    source,
                });
            }
        };
        let published = if existing {
            unsafe {
                windows_sys::Win32::Storage::FileSystem::ReplaceFileW(
                    destination.as_ptr(),
                    replacement.as_ptr(),
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    std::ptr::null(),
                )
            }
        } else {
            unsafe {
                windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                    replacement.as_ptr(),
                    destination.as_ptr(),
                    windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
                )
            }
        };
        if published == 0 {
            return Err(StateError::Io {
                operation: "atomically publish process state",
                source: std::io::Error::last_os_error(),
            });
        }
        cleanup.disarm();
        self.inject(StateFailurePoint::SyncParent)
    }
}

#[cfg(target_os = "linux")]
fn open_state_for_read(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    validate_private_regular_linux(&file)?;
    Ok(file)
}

#[cfg(target_os = "linux")]
fn validate_private_regular_linux(file: &File) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "state path is not a regular file",
        ));
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "state file is not owned by the current user",
        ));
    }
    if metadata.permissions().mode() & 0o777 != 0o600 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "state file permissions are not exactly 0600",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_directory_linux(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

#[cfg(windows)]
fn open_state_for_read(path: &Path) -> std::io::Result<File> {
    use std::os::windows::io::FromRawHandle;
    let security = OwnerOnlySecurity::new()?;
    let handle = create_private_file(path, &security, false)?;
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(all(test, windows))]
pub(crate) fn validate_private_test_path(path: &Path) -> std::io::Result<()> {
    let wide = wide_path(path).map_err(state_to_io)?;
    let handle = unsafe {
        windows_sys::Win32::Storage::FileSystem::CreateFileW(
            wide.as_ptr(),
            windows_sys::Win32::Foundation::GENERIC_READ,
            windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
                | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE,
            std::ptr::null(),
            windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING,
            windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL
                | windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let result = validate_private_regular_windows(handle);
    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
    result
}

#[cfg(not(any(windows, target_os = "linux")))]
fn open_state_for_read(_path: &Path) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "processctl state supports only Windows and Linux",
    ))
}

struct TempCleanup<'a> {
    path: &'a Path,
    armed: bool,
}

impl<'a> TempCleanup<'a> {
    fn new(path: &'a Path) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempCleanup<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(self.path);
        }
    }
}

#[cfg(windows)]
pub(crate) struct OwnerOnlySecurity {
    descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
}

#[cfg(windows)]
impl OwnerOnlySecurity {
    pub(crate) fn new() -> std::io::Result<Self> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };

        let sid = current_user_sid_string()?;
        let sddl = format!("O:{sid}D:P(A;;GA;;;{sid})");
        let sddl: Vec<u16> = std::ffi::OsStr::new(&sddl)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut descriptor = std::ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = std::ptr::null_mut();
        if unsafe {
            windows_sys::Win32::Security::GetSecurityDescriptorDacl(
                descriptor,
                &mut present,
                &mut dacl,
                &mut defaulted,
            )
        } == 0
            || present == 0
            || dacl.is_null()
        {
            unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor as _) };
            return Err(std::io::Error::last_os_error());
        }
        let mut owner = std::ptr::null_mut();
        let mut owner_defaulted = 0;
        if unsafe {
            windows_sys::Win32::Security::GetSecurityDescriptorOwner(
                descriptor,
                &mut owner,
                &mut owner_defaulted,
            )
        } == 0
            || owner.is_null()
        {
            unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor as _) };
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { descriptor })
    }

    pub(crate) fn attributes(&self) -> windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
        windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>()
                as u32,
            lpSecurityDescriptor: self.descriptor,
            bInheritHandle: 0,
        }
    }
}

#[cfg(windows)]
pub(crate) fn current_user_sid_string() -> std::io::Result<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let result = (|| {
        let mut required = 0;
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut required) };
        if required == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let words = required.div_ceil(std::mem::size_of::<usize>() as u32) as usize;
        let mut buffer = vec![0usize; words];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
        sid_to_string(user.User.Sid)
    })();
    unsafe { CloseHandle(token) };
    result
}

#[cfg(windows)]
pub(crate) fn sid_to_string(sid: windows_sys::Win32::Security::PSID) -> std::io::Result<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    let mut sid_string = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut sid_string) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let length = (0..)
        .find(|&index| unsafe { *sid_string.add(index) } == 0)
        .expect("Windows SID string is NUL terminated");
    let result = String::from_utf16(unsafe { std::slice::from_raw_parts(sid_string, length) })
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error));
    unsafe { LocalFree(sid_string.cast()) };
    result
}

#[cfg(windows)]
impl Drop for OwnerOnlySecurity {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::LocalFree(self.descriptor as _) };
    }
}

#[cfg(windows)]
fn create_private_file(
    path: &Path,
    security: &OwnerOnlySecurity,
    create_new: bool,
) -> std::io::Result<windows_sys::Win32::Foundation::HANDLE> {
    let path = wide_path(path).map_err(state_to_io)?;
    let attributes = security.attributes();
    let handle = unsafe {
        windows_sys::Win32::Storage::FileSystem::CreateFileW(
            path.as_ptr(),
            windows_sys::Win32::Foundation::GENERIC_READ
                | windows_sys::Win32::Foundation::GENERIC_WRITE,
            windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ,
            &attributes,
            if create_new {
                windows_sys::Win32::Storage::FileSystem::CREATE_NEW
            } else {
                windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING
            },
            windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL
                | windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        Err(std::io::Error::last_os_error())
    } else {
        let validation = validate_private_regular_windows(handle);
        if let Err(error) = validation {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            Err(error)
        } else {
            Ok(handle)
        }
    }
}

#[cfg(windows)]
pub(crate) fn validate_private_regular_windows(
    handle: windows_sys::Win32::Foundation::HANDLE,
) -> std::io::Result<()> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        AclSizeInformation, GetAce, GetAclInformation, GetSecurityDescriptorControl,
        ACCESS_ALLOWED_ACE, ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION,
        OWNER_SECURITY_INFORMATION, SE_DACL_PROTECTED,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ALL_ACCESS,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    };

    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "processctl path is not a regular non-reparse file",
        ));
    }

    let mut owner = std::ptr::null_mut();
    let mut dacl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    let result = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if result != 0 {
        return Err(std::io::Error::from_raw_os_error(result as i32));
    }
    let validation = (|| {
        if owner.is_null() || dacl.is_null() || sid_to_string(owner)? != current_user_sid_string()?
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "processctl file owner is not the current user",
            ));
        }
        let mut control = 0;
        let mut revision = 0;
        if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
            || control & SE_DACL_PROTECTED == 0
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "processctl file DACL is not protected",
            ));
        }
        let mut acl_info: ACL_SIZE_INFORMATION = unsafe { zeroed() };
        if unsafe {
            GetAclInformation(
                dacl,
                (&raw mut acl_info).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        } == 0
            || acl_info.AceCount != 1
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "processctl file DACL must contain exactly one ACE",
            ));
        }
        let mut ace = std::ptr::null_mut();
        if unsafe { GetAce(dacl, 0, &mut ace) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let ace = unsafe { &*(ace.cast::<ACCESS_ALLOWED_ACE>()) };
        let ace_sid = (&raw const ace.SidStart).cast_mut().cast();
        if ace.Header.AceType != 0
            || ace.Header.AceFlags != 0
            || ace.Mask != FILE_ALL_ACCESS
            || sid_to_string(ace_sid)? != current_user_sid_string()?
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "processctl file DACL is not one current-user GENERIC_ALL ACE",
            ));
        }
        Ok(())
    })();
    unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor as _) };
    validation
}

#[cfg(windows)]
pub(crate) fn wide_path(path: &Path) -> Result<Vec<u16>, StateError> {
    use std::os::windows::ffi::OsStrExt;
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(StateError::InvalidField {
            field: "state path",
            reason: "contains NUL",
        });
    }
    wide.push(0);
    Ok(wide)
}

#[cfg(windows)]
fn state_to_io(error: StateError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, error)
}

pub(crate) fn validate_identifier(field: &'static str, value: &str) -> Result<(), StateError> {
    if value.is_empty() || value.len() > 128 {
        return Err(StateError::InvalidField {
            field,
            reason: "must contain 1..=128 bytes",
        });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(StateError::InvalidField {
            field,
            reason: "may contain only ASCII letters, digits, dot, underscore, and dash",
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StateFailurePoint {
    CreateTemp,
    SecureTemp,
    Write,
    Flush,
    #[cfg(windows)]
    CrashBeforePublish,
    Replace,
    SyncParent,
}

#[cfg(test)]
impl StateStore {
    pub(crate) fn failing(path: impl Into<PathBuf>, failure: StateFailurePoint) -> Self {
        Self {
            path: path.into(),
            failure: Some(failure),
        }
    }
}
