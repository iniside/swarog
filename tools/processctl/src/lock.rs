use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::state::validate_identifier;
use crate::{observe_process_identity, OwnedChild, ProcessIdentity, SpawnSpec};

pub const ROLLOUT_LOCK_VERSION: u32 = 1;
const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;
const MAX_METADATA_BYTES: u64 = 64 * 1024;
static INHERITED_CREDENTIAL_CONSUMED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct LockMetadata {
    version: u32,
    owner: ProcessIdentity,
    run_id: String,
    lease_started_unix_nanos: u64,
    allowed_borrower_role: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct BorrowCredential {
    version: u32,
    lock_path: PathBuf,
    metadata: LockMetadata,
    nonce: [u8; 32],
}

#[derive(Debug, Error)]
pub enum LeaseError {
    #[error("invalid rollout lock field: {0}")]
    InvalidField(String),
    #[error("rollout lock is already owned")]
    AlreadyOwned,
    #[error("borrower credential was already issued")]
    BorrowerAlreadyIssued,
    #[error("borrower role mismatch: expected {expected}, received {received}")]
    WrongRole { expected: String, received: String },
    #[error("rollout lock owner is not live or no longer owns the advisory lock")]
    OwnerNotLive,
    #[error("rollout lock metadata does not match the inherited credential")]
    MetadataMismatch,
    #[error("borrower credential was already consumed")]
    BorrowerReplay,
    #[error("unsupported rollout lock version {0}")]
    UnsupportedVersion(u32),
    #[error("serialize rollout lock data: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("{operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("spawn borrowed process: {0}")]
    Spawn(#[from] crate::ProcessError),
}

pub struct RolloutLock;

pub struct OwnedLease {
    file: File,
    path: PathBuf,
    metadata: LockMetadata,
    borrower_issued: bool,
}

pub struct BorrowedLease {
    _lock_file: File,
    metadata: LockMetadata,
    _not_transferable: PhantomData<Rc<()>>,
}

impl RolloutLock {
    pub fn acquire(
        path: impl Into<PathBuf>,
        run_id: impl Into<String>,
        allowed_borrower_role: impl Into<String>,
    ) -> Result<OwnedLease, LeaseError> {
        let path = path.into();
        let run_id = run_id.into();
        let allowed_borrower_role = allowed_borrower_role.into();
        validate_identifier("run id", &run_id)
            .map_err(|error| LeaseError::InvalidField(error.to_string()))?;
        validate_identifier("borrower role", &allowed_borrower_role)
            .map_err(|error| LeaseError::InvalidField(error.to_string()))?;
        let parent = path.parent().ok_or_else(|| {
            LeaseError::InvalidField("lock path must have a parent directory".into())
        })?;
        if !parent.is_dir() {
            return Err(LeaseError::InvalidField(
                "lock parent directory does not exist".into(),
            ));
        }

        let mut file = open_lock_file(&path)?;
        if !try_lock_exclusive(&file)? {
            return Err(LeaseError::AlreadyOwned);
        }
        let owner = match observe_process_identity(std::process::id()) {
            Ok(owner) => owner,
            Err(error) => {
                let _ = unlock(&file);
                return Err(LeaseError::InvalidField(error.to_string()));
            }
        };
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| LeaseError::InvalidField(error.to_string()))?
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64;
        let metadata = LockMetadata {
            version: ROLLOUT_LOCK_VERSION,
            owner,
            run_id,
            lease_started_unix_nanos: started,
            allowed_borrower_role,
        };
        if let Err(error) = write_metadata(&mut file, &metadata) {
            let _ = unlock(&file);
            return Err(error);
        }
        Ok(OwnedLease {
            file,
            path,
            metadata,
            borrower_issued: false,
        })
    }
}

impl OwnedLease {
    pub fn owner(&self) -> &ProcessIdentity {
        &self.metadata.owner
    }

    pub fn run_id(&self) -> &str {
        &self.metadata.run_id
    }

    pub fn allowed_borrower_role(&self) -> &str {
        &self.metadata.allowed_borrower_role
    }

    pub fn spawn_borrower(
        &mut self,
        spec: SpawnSpec,
        role: &str,
    ) -> Result<OwnedChild, LeaseError> {
        if self.borrower_issued {
            return Err(LeaseError::BorrowerAlreadyIssued);
        }
        if role != self.metadata.allowed_borrower_role {
            return Err(LeaseError::WrongRole {
                expected: self.metadata.allowed_borrower_role.clone(),
                received: role.to_string(),
            });
        }
        self.borrower_issued = true;
        let mut nonce = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let credential = self.credential(nonce);
        let bytes = serde_json::to_vec(&credential)?;
        if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
            return Err(LeaseError::InvalidField(
                "borrower credential exceeds its bound".into(),
            ));
        }
        let (input, mut writer) = credential_pipe()?;
        writer.write_all(&bytes).map_err(|source| LeaseError::Io {
            operation: "write one-shot borrower credential",
            source,
        })?;
        writer.flush().map_err(|source| LeaseError::Io {
            operation: "flush one-shot borrower credential",
            source,
        })?;
        drop(writer);
        Ok(OwnedChild::spawn_with_input(spec, input)?)
    }

    fn credential(&self, nonce: [u8; 32]) -> BorrowCredential {
        BorrowCredential {
            version: ROLLOUT_LOCK_VERSION,
            lock_path: self.path.clone(),
            metadata: self.metadata.clone(),
            nonce,
        }
    }

    #[cfg(test)]
    pub(crate) fn credential_for_test(&self) -> BorrowCredential {
        self.credential([7; 32])
    }
}

impl Drop for OwnedLease {
    fn drop(&mut self) {
        let _ = unlock(&self.file);
    }
}

impl BorrowedLease {
    pub fn consume_inherited(expected_role: &str) -> Result<Self, LeaseError> {
        validate_identifier("borrower role", expected_role)
            .map_err(|error| LeaseError::InvalidField(error.to_string()))?;
        if INHERITED_CREDENTIAL_CONSUMED.swap(true, Ordering::AcqRel) {
            return Err(LeaseError::BorrowerReplay);
        }
        let bytes = consume_credential_stdin()?;
        let credential: BorrowCredential = serde_json::from_slice(&bytes)?;
        validate_credential(credential, expected_role)
    }

    pub fn owner(&self) -> &ProcessIdentity {
        &self.metadata.owner
    }

    pub fn run_id(&self) -> &str {
        &self.metadata.run_id
    }
}

pub(crate) fn validate_credential(
    credential: BorrowCredential,
    expected_role: &str,
) -> Result<BorrowedLease, LeaseError> {
    if credential.version != ROLLOUT_LOCK_VERSION {
        return Err(LeaseError::UnsupportedVersion(credential.version));
    }
    if credential.metadata.allowed_borrower_role != expected_role {
        return Err(LeaseError::WrongRole {
            expected: credential.metadata.allowed_borrower_role,
            received: expected_role.to_string(),
        });
    }
    let mut lock_file = open_lock_file(&credential.lock_path)?;
    let metadata = read_metadata(&mut lock_file)?;
    if metadata != credential.metadata {
        return Err(LeaseError::MetadataMismatch);
    }
    let observed =
        observe_process_identity(metadata.owner.pid).map_err(|_| LeaseError::OwnerNotLive)?;
    if observed != metadata.owner || !is_locked_by_other(&lock_file)? {
        return Err(LeaseError::OwnerNotLive);
    }
    let marker = borrow_marker_path(&credential.lock_path, &metadata);
    create_consumption_marker(&marker, &credential.nonce)?;
    Ok(BorrowedLease {
        _lock_file: lock_file,
        metadata,
        _not_transferable: PhantomData,
    })
}

fn write_metadata(file: &mut File, metadata: &LockMetadata) -> Result<(), LeaseError> {
    let bytes = serde_json::to_vec_pretty(metadata)?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(LeaseError::InvalidField(
            "rollout lock metadata exceeds its bound".into(),
        ));
    }
    file.set_len(0).map_err(|source| LeaseError::Io {
        operation: "truncate rollout lock metadata",
        source,
    })?;
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.write_all(&bytes))
        .and_then(|_| file.flush())
        .map_err(|source| LeaseError::Io {
            operation: "write rollout lock metadata",
            source,
        })?;
    flush_file(file, "flush rollout lock metadata")
}

fn read_metadata(file: &mut File) -> Result<LockMetadata, LeaseError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| LeaseError::Io {
            operation: "seek rollout lock metadata",
            source,
        })?;
    let mut bytes = Vec::new();
    file.take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| LeaseError::Io {
            operation: "read rollout lock metadata",
            source,
        })?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(LeaseError::InvalidField(
            "rollout lock metadata exceeds its bound".into(),
        ));
    }
    let metadata: LockMetadata = serde_json::from_slice(&bytes)?;
    if metadata.version != ROLLOUT_LOCK_VERSION {
        return Err(LeaseError::UnsupportedVersion(metadata.version));
    }
    Ok(metadata)
}

fn borrow_marker_path(lock: &Path, metadata: &LockMetadata) -> PathBuf {
    let file_name = lock
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    lock.with_file_name(format!(
        ".{file_name}.{}.{}.borrowed",
        metadata.run_id, metadata.lease_started_unix_nanos
    ))
}

#[cfg(target_os = "linux")]
fn open_lock_file(path: &Path) -> Result<File, LeaseError> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| LeaseError::Io {
            operation: "open private rollout lock",
            source,
        })?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
        LeaseError::Io {
            operation: "secure rollout lock permissions",
            source,
        }
    })?;
    Ok(file)
}

#[cfg(windows)]
fn open_lock_file(path: &Path) -> Result<File, LeaseError> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_ALWAYS,
    };

    let security = crate::state::OwnerOnlySecurity::new().map_err(|source| LeaseError::Io {
        operation: "build rollout lock owner DACL",
        source,
    })?;
    let path_wide = crate::state::wide_path(path)
        .map_err(|error| LeaseError::InvalidField(error.to_string()))?;
    let attributes = security.attributes();
    let handle = unsafe {
        CreateFileW(
            path_wide.as_ptr(),
            windows_sys::Win32::Foundation::GENERIC_READ
                | windows_sys::Win32::Foundation::GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &attributes,
            OPEN_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return Err(LeaseError::Io {
            operation: "open private rollout lock",
            source: std::io::Error::last_os_error(),
        });
    }
    if let Err(source) = crate::state::apply_owner_only_dacl(path, &security) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        return Err(LeaseError::Io {
            operation: "secure rollout lock DACL",
            source,
        });
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(target_os = "linux")]
fn try_lock_exclusive(file: &File) -> Result<bool, LeaseError> {
    use std::os::fd::AsRawFd;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        Ok(true)
    } else {
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN)
        {
            Ok(false)
        } else {
            Err(LeaseError::Io {
                operation: "acquire rollout lock",
                source: error,
            })
        }
    }
}

#[cfg(windows)]
fn try_lock_exclusive(file: &File) -> Result<bool, LeaseError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    let mut overlapped = lock_overlapped();
    let result = unsafe {
        LockFileEx(
            file.as_raw_handle() as _,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    };
    if result != 0 {
        Ok(true)
    } else {
        let error = std::io::Error::last_os_error();
        const ERROR_LOCK_VIOLATION: i32 = 33;
        if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION) {
            Ok(false)
        } else {
            Err(LeaseError::Io {
                operation: "acquire rollout lock",
                source: error,
            })
        }
    }
}

fn is_locked_by_other(file: &File) -> Result<bool, LeaseError> {
    if try_lock_exclusive(file)? {
        unlock(file)?;
        Ok(false)
    } else {
        Ok(true)
    }
}

#[cfg(target_os = "linux")]
fn unlock(file: &File) -> Result<(), LeaseError> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0 {
        Ok(())
    } else {
        Err(LeaseError::Io {
            operation: "release rollout lock",
            source: std::io::Error::last_os_error(),
        })
    }
}

#[cfg(windows)]
fn unlock(file: &File) -> Result<(), LeaseError> {
    use std::os::windows::io::AsRawHandle;
    let mut overlapped = lock_overlapped();
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::UnlockFileEx(
            file.as_raw_handle() as _,
            0,
            1,
            0,
            &mut overlapped,
        )
    } != 0
    {
        Ok(())
    } else {
        Err(LeaseError::Io {
            operation: "release rollout lock",
            source: std::io::Error::last_os_error(),
        })
    }
}

#[cfg(windows)]
fn lock_overlapped() -> windows_sys::Win32::System::IO::OVERLAPPED {
    let mut overlapped: windows_sys::Win32::System::IO::OVERLAPPED = unsafe { std::mem::zeroed() };
    let offset = 1u64 << 63;
    overlapped.Anonymous.Anonymous.Offset = offset as u32;
    overlapped.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
    overlapped
}

#[cfg(target_os = "linux")]
fn flush_file(file: &File, operation: &'static str) -> Result<(), LeaseError> {
    file.sync_all()
        .map_err(|source| LeaseError::Io { operation, source })
}

#[cfg(windows)]
fn flush_file(file: &File, operation: &'static str) -> Result<(), LeaseError> {
    use std::os::windows::io::AsRawHandle;
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::FlushFileBuffers(file.as_raw_handle() as _)
    } != 0
    {
        Ok(())
    } else {
        Err(LeaseError::Io {
            operation,
            source: std::io::Error::last_os_error(),
        })
    }
}

#[cfg(target_os = "linux")]
fn credential_pipe() -> Result<(crate::platform::InheritedInput, File), LeaseError> {
    use std::os::fd::FromRawFd;
    let mut fds = [-1; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(LeaseError::Io {
            operation: "create one-shot borrower pipe",
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(unsafe {
        (
            crate::platform::InheritedInput(File::from_raw_fd(fds[0])),
            File::from_raw_fd(fds[1]),
        )
    })
}

#[cfg(windows)]
fn credential_pipe() -> Result<(crate::platform::InheritedInput, File), LeaseError> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Pipes::CreatePipe;
    let mut attributes: SECURITY_ATTRIBUTES = unsafe { std::mem::zeroed() };
    attributes.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32;
    attributes.bInheritHandle = 1;
    let mut read = std::ptr::null_mut();
    let mut write = std::ptr::null_mut();
    if unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) } == 0 {
        return Err(LeaseError::Io {
            operation: "create one-shot borrower pipe",
            source: std::io::Error::last_os_error(),
        });
    }
    if unsafe { SetHandleInformation(write, HANDLE_FLAG_INHERIT, 0) } == 0 {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(read);
            windows_sys::Win32::Foundation::CloseHandle(write);
        }
        return Err(LeaseError::Io {
            operation: "make borrower writer non-inheritable",
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(unsafe {
        (
            crate::platform::InheritedInput(File::from_raw_handle(read)),
            File::from_raw_handle(write),
        )
    })
}

#[cfg(target_os = "linux")]
fn consume_credential_stdin() -> Result<Vec<u8>, LeaseError> {
    use std::os::fd::{FromRawFd, IntoRawFd};
    if unsafe { libc::fcntl(0, libc::F_GETFD) } < 0 {
        return Err(LeaseError::Io {
            operation: "open inherited borrower credential",
            source: std::io::Error::last_os_error(),
        });
    }
    let mut input = unsafe { File::from_raw_fd(0) };
    let bytes = read_credential_to_eof(&mut input)?;
    drop(input);

    let null = File::open("/dev/null").map_err(|source| LeaseError::Io {
        operation: "replace consumed borrower credential",
        source,
    })?;
    let null_fd = null.into_raw_fd();
    if null_fd != 0 {
        if unsafe { libc::dup2(null_fd, 0) } < 0 {
            let source = std::io::Error::last_os_error();
            unsafe { libc::close(null_fd) };
            return Err(LeaseError::Io {
                operation: "replace consumed borrower credential",
                source,
            });
        }
        unsafe { libc::close(null_fd) };
    }
    if unsafe { libc::fcntl(0, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(LeaseError::Io {
            operation: "seal consumed borrower credential descriptor",
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(bytes)
}

#[cfg(windows)]
fn consume_credential_stdin() -> Result<Vec<u8>, LeaseError> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::System::Console::{GetStdHandle, SetStdHandle, STD_INPUT_HANDLE};
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle.is_null() || handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return Err(LeaseError::Io {
            operation: "open inherited borrower credential",
            source: std::io::Error::last_os_error(),
        });
    }
    unsafe { SetStdHandle(STD_INPUT_HANDLE, std::ptr::null_mut()) };
    let mut input = unsafe { File::from_raw_handle(handle) };
    read_credential_to_eof(&mut input)
}

fn read_credential_to_eof(input: &mut File) -> Result<Vec<u8>, LeaseError> {
    let mut bytes = Vec::new();
    input
        .take(MAX_CREDENTIAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| LeaseError::Io {
            operation: "consume inherited borrower credential",
            source,
        })?;
    if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
        return Err(LeaseError::InvalidField(
            "borrower credential exceeds its bound".into(),
        ));
    }
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn create_consumption_marker(path: &Path, nonce: &[u8; 32]) -> Result<(), LeaseError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::AlreadyExists {
                LeaseError::BorrowerReplay
            } else {
                LeaseError::Io {
                    operation: "create one-shot borrower marker",
                    source,
                }
            }
        })?;
    file.write_all(nonce).map_err(|source| LeaseError::Io {
        operation: "write one-shot borrower marker",
        source,
    })?;
    flush_file(&file, "flush one-shot borrower marker")
}

#[cfg(windows)]
fn create_consumption_marker(path: &Path, nonce: &[u8; 32]) -> Result<(), LeaseError> {
    use std::os::windows::io::FromRawHandle;
    let security = crate::state::OwnerOnlySecurity::new().map_err(|source| LeaseError::Io {
        operation: "build borrower marker owner DACL",
        source,
    })?;
    let handle = super_private_create_new(path, &security).map_err(|source| {
        if source.kind() == std::io::ErrorKind::AlreadyExists {
            LeaseError::BorrowerReplay
        } else {
            LeaseError::Io {
                operation: "create one-shot borrower marker",
                source,
            }
        }
    })?;
    let mut file = unsafe { File::from_raw_handle(handle) };
    file.write_all(nonce).map_err(|source| LeaseError::Io {
        operation: "write one-shot borrower marker",
        source,
    })?;
    flush_file(&file, "flush one-shot borrower marker")
}

#[cfg(windows)]
fn super_private_create_new(
    path: &Path,
    security: &crate::state::OwnerOnlySecurity,
) -> std::io::Result<windows_sys::Win32::Foundation::HANDLE> {
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
    };
    let path = crate::state::wide_path(path).map_err(|error| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
    })?;
    let attributes = security.attributes();
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            windows_sys::Win32::Foundation::GENERIC_READ
                | windows_sys::Win32::Foundation::GENERIC_WRITE,
            FILE_SHARE_READ,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(handle)
    }
}
