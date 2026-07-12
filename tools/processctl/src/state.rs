use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{OwnedChild, ProcessIdentity, ShutdownPolicy};

pub const STATE_VERSION: u32 = 1;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

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
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ManagedStatus {
    Starting,
    Healthy,
    Stopping,
    Exited { code: Option<i32> },
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
pub struct FleetState {
    version: u32,
    run_id: String,
    topology: String,
    status: FleetStatus,
    control_endpoint: Option<PathBuf>,
    processes: Vec<ManagedProcess>,
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
            control_endpoint: None,
            processes: Vec::new(),
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
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StateError::Io {
                    operation: "read process state",
                    source,
                });
            }
        };
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
            .open(temp)
            .map_err(|source| StateError::Io {
                operation: "create private state temp file",
                source,
            })?;
        let mut cleanup = TempCleanup::new(temp);
        std::fs::set_permissions(temp, std::fs::Permissions::from_mode(0o600)).map_err(
            |source| StateError::Io {
                operation: "secure state temp file",
                source,
            },
        )?;
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
        std::fs::File::open(parent)
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

        let mut new_destination_cleanup = None;
        if self.path.exists() {
            apply_owner_only_dacl(&self.path, &security).map_err(|source| StateError::Io {
                operation: "secure existing state destination",
                source,
            })?;
        } else {
            let placeholder =
                create_private_file(&self.path, &security, true).map_err(|source| {
                    StateError::Io {
                        operation: "create owner-only state destination",
                        source,
                    }
                })?;
            new_destination_cleanup = Some(TempCleanup::new(&self.path));
            if unsafe { windows_sys::Win32::Storage::FileSystem::FlushFileBuffers(placeholder) }
                == 0
            {
                unsafe { windows_sys::Win32::Foundation::CloseHandle(placeholder) };
                return Err(StateError::Io {
                    operation: "flush state destination placeholder",
                    source: std::io::Error::last_os_error(),
                });
            }
            unsafe { windows_sys::Win32::Foundation::CloseHandle(placeholder) };
        }

        self.inject(StateFailurePoint::Replace)?;
        let destination = wide_path(&self.path)?;
        let replacement = wide_path(temp)?;
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::ReplaceFileW(
                destination.as_ptr(),
                replacement.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(StateError::Io {
                operation: "atomically replace process state",
                source: std::io::Error::last_os_error(),
            });
        }
        cleanup.disarm();
        if let Some(cleanup) = new_destination_cleanup.as_mut() {
            cleanup.disarm();
        }
        self.inject(StateFailurePoint::SyncParent)
    }
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
struct OwnerOnlySecurity {
    descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
    dacl: *mut windows_sys::Win32::Security::ACL,
}

#[cfg(windows)]
impl OwnerOnlySecurity {
    fn new() -> std::io::Result<Self> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };

        let sddl: Vec<u16> = std::ffi::OsStr::new("D:P(A;;GA;;;OW)")
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
        Ok(Self { descriptor, dacl })
    }

    fn attributes(&self) -> windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
        windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>()
                as u32,
            lpSecurityDescriptor: self.descriptor,
            bInheritHandle: 0,
        }
    }
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
            windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(handle)
    }
}

#[cfg(windows)]
fn apply_owner_only_dacl(path: &Path, security: &OwnerOnlySecurity) -> std::io::Result<()> {
    use windows_sys::Win32::Security::Authorization::{SetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    };

    let mut path = wide_path(path).map_err(state_to_io)?;
    let result = unsafe {
        SetNamedSecurityInfoW(
            path.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            security.dacl,
            std::ptr::null(),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(result as i32))
    }
}

#[cfg(windows)]
fn wide_path(path: &Path) -> Result<Vec<u16>, StateError> {
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

fn validate_identifier(field: &'static str, value: &str) -> Result<(), StateError> {
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
