//! `run/rollout.lock` participation — the ONE convention weles shares with
//! devctl/verifyctl/processctl: at most one rollout-bearing command may run
//! against the shared local Postgres at a time. The locking protocol is
//! copied (never imported — zero-sharing) from `tools/processctl/src/lock.rs`
//! and must stay bit-compatible with it:
//!
//! * Unix: `flock(LOCK_EX | LOCK_NB)` on the whole file.
//! * Windows: `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY)`
//!   on EXACTLY 1 byte at offset `1 << 63` (`tools/processctl/src/lock.rs`,
//!   `try_lock_exclusive`/`lock_overlapped`), with the file opened
//!   `FILE_SHARE_READ | FILE_SHARE_WRITE`. Locking any other range would let
//!   weles and a devctl/verifyctl rollout both "acquire" the lock at once.
//! * If weles CREATES the file on Windows, it must carry the owner-only,
//!   `SE_DACL_PROTECTED` DACL (`tools/processctl/src/state.rs`,
//!   `OwnerOnlySecurity`) — a plain `std::fs`-created lock file would make
//!   every later devctl/verifyctl run fail its lock-security validation
//!   permanently. On Unix the file is created mode 0600.
//!
//! After acquiring, the file is truncated and rewritten with weles's OWN
//! metadata schema (`{version, tool, pid, run_id, started_unix}`); devctl
//! never reads foreign metadata — it truncates on its own acquire the same
//! way.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::Serialize;

/// weles's own lock-metadata schema. Purely informational for a human (or a
/// later `weles status`) inspecting who owns the rollout.
#[derive(Serialize)]
struct LockMetadata<'a> {
    version: u32,
    tool: &'a str,
    pid: u32,
    run_id: &'a str,
    started_unix: u64,
}

/// RAII ownership of `run/rollout.lock`, held for the entire `up()` lifetime.
/// Dropping it releases the byte-range/flock lock and closes the handle.
#[derive(Debug)]
pub struct RolloutLock {
    file: File,
    path: PathBuf,
}

impl RolloutLock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RolloutLock {
    fn drop(&mut self) {
        // Best-effort explicit unlock; closing the handle releases the lock
        // anyway on both platforms.
        let _ = imp::unlock(&self.file);
    }
}

/// Acquires `<root>/run/rollout.lock` exclusively and non-blockingly
/// (creating `run/` if missing), then truncates and rewrites the metadata.
/// A lock owned by anyone else — devctl, verifyctl, or another weles — is a
/// loud, immediate error naming the path.
pub fn acquire(root: &Path, run_id: &str) -> Result<RolloutLock> {
    let run_dir = root.join("run");
    std::fs::create_dir_all(&run_dir)
        .with_context(|| format!("create lock directory {}", run_dir.display()))?;
    let path = run_dir.join("rollout.lock");

    let mut file =
        imp::open_lock_file(&path).with_context(|| format!("open {}", path.display()))?;

    let acquired = imp::try_lock_exclusive(&file)
        .with_context(|| format!("acquire rollout lock {}", path.display()))?;
    if !acquired {
        bail!(
            "another rollout owns {} — a devctl/verifyctl/weles rollout is active on this \
             machine (one test rollout at a time on the shared Postgres); stop it or wait \
             for it to finish before running weles up",
            path.display()
        );
    }

    let started_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let metadata = serde_json::to_vec_pretty(&LockMetadata {
        version: 1,
        tool: "weles",
        pid: std::process::id(),
        run_id,
        started_unix,
    })
    .context("serialize rollout lock metadata")?;

    // The locked byte lives at offset 1<<63; the metadata at offset 0 — the
    // regions never overlap, and the lock-owning handle may write freely.
    file.set_len(0)
        .with_context(|| format!("truncate {}", path.display()))?;
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("rewind {}", path.display()))?;
    file.write_all(&metadata)
        .with_context(|| format!("write metadata to {}", path.display()))?;
    file.flush()
        .with_context(|| format!("flush metadata to {}", path.display()))?;

    Ok(RolloutLock { file, path })
}

#[cfg(unix)]
mod imp {
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::path::Path;

    /// Opens (creating mode-0600 if absent) the lock file. `mode` applies
    /// only at creation; an existing file (e.g. devctl's) keeps its own.
    pub(super) fn open_lock_file(path: &Path) -> std::io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
    }

    /// `flock(LOCK_EX | LOCK_NB)` — `Ok(false)` when someone else holds it.
    /// flock is per open-file-description, so even a second handle in the
    /// SAME process contends (pinned by `lock_tests`).
    pub(super) fn try_lock_exclusive(file: &File) -> std::io::Result<bool> {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(false),
            _ => Err(error),
        }
    }

    pub(super) fn unlock(file: &File) -> std::io::Result<()> {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::fs::File;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::Path;

    use windows_sys::Win32::Foundation::{
        CloseHandle, LocalFree, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, GetTokenInformation, TokenUser,
        PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, LockFileEx, UnlockFileEx, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
        FILE_SHARE_WRITE, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, OPEN_ALWAYS,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// Opens (or creates, carrying the owner-only protected DACL) the lock
    /// file with `FILE_SHARE_READ | FILE_SHARE_WRITE` — the same sharing mode
    /// processctl/devctl use, so concurrent opens contend only on the
    /// byte-range lock, never on the open itself. `SECURITY_ATTRIBUTES` only
    /// applies when the file is actually created; an existing file keeps the
    /// DACL its creator gave it.
    pub(super) fn open_lock_file(path: &Path) -> std::io::Result<File> {
        let security = OwnerOnlySecurity::new()?;
        let path = wide_path(path)?;
        let attributes = security.attributes();
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &attributes,
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_handle(handle) })
    }

    /// `LockFileEx` on EXACTLY 1 byte at offset `1 << 63` — bit-compatible
    /// with `tools/processctl/src/lock.rs::try_lock_exclusive`. Any other
    /// range would not contend with a devctl/verifyctl rollout at all.
    pub(super) fn try_lock_exclusive(file: &File) -> std::io::Result<bool> {
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
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        const ERROR_LOCK_VIOLATION: i32 = 33;
        if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION) {
            Ok(false)
        } else {
            Err(error)
        }
    }

    pub(super) fn unlock(file: &File) -> std::io::Result<()> {
        let mut overlapped = lock_overlapped();
        if unsafe { UnlockFileEx(file.as_raw_handle() as _, 0, 1, 0, &mut overlapped) } != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    fn lock_overlapped() -> windows_sys::Win32::System::IO::OVERLAPPED {
        let mut overlapped: windows_sys::Win32::System::IO::OVERLAPPED =
            unsafe { std::mem::zeroed() };
        let offset = 1u64 << 63;
        overlapped.Anonymous.Anonymous.Offset = offset as u32;
        overlapped.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
        overlapped
    }

    /// Owner-only, protected security descriptor
    /// (`O:<sid>D:P(A;;GA;;;<sid>)`) applied when weles CREATES the lock
    /// file. Copied from `tools/processctl/src/state.rs::OwnerOnlySecurity`
    /// so a weles-created lock file passes processctl/devctl's later
    /// owner/DACL validation.
    struct OwnerOnlySecurity {
        descriptor: PSECURITY_DESCRIPTOR,
    }

    impl OwnerOnlySecurity {
        fn new() -> std::io::Result<Self> {
            use std::os::windows::ffi::OsStrExt;

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
            // Sanity: the SDDL round-trip must have produced both a DACL and
            // an owner (guards against a silent SDDL formatting mistake).
            let mut present = 0;
            let mut defaulted = 0;
            let mut dacl = std::ptr::null_mut();
            if unsafe {
                GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
            } == 0
                || present == 0
                || dacl.is_null()
            {
                unsafe { LocalFree(descriptor as _) };
                return Err(std::io::Error::last_os_error());
            }
            let mut owner = std::ptr::null_mut();
            let mut owner_defaulted = 0;
            if unsafe {
                GetSecurityDescriptorOwner(descriptor, &mut owner, &mut owner_defaulted)
            } == 0
                || owner.is_null()
            {
                unsafe { LocalFree(descriptor as _) };
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self { descriptor })
        }

        fn attributes(&self) -> SECURITY_ATTRIBUTES {
            SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.descriptor,
                bInheritHandle: 0,
            }
        }
    }

    impl Drop for OwnerOnlySecurity {
        fn drop(&mut self) {
            unsafe { LocalFree(self.descriptor as _) };
        }
    }

    fn current_user_sid_string() -> std::io::Result<String> {
        let mut token = std::ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let result = (|| {
            let mut required = 0;
            unsafe {
                GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut required)
            };
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

    fn sid_to_string(sid: PSID) -> std::io::Result<String> {
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

    fn wide_path(path: &Path) -> std::io::Result<Vec<u16>> {
        use std::os::windows::ffi::OsStrExt;
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        if wide.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "lock path contains NUL",
            ));
        }
        wide.push(0);
        Ok(wide)
    }
}

#[cfg(test)]
#[path = "lock_tests.rs"]
mod lock_tests;
