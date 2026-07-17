use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use processctl::{FleetState, ProcessIdentity};
use serde::{Deserialize, Serialize};

const MAX_FRAME: usize = 4096;
const IO_DEADLINE: Duration = Duration::from_secs(2);
const BIND_DEADLINE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(10);

#[derive(Deserialize, Serialize)]
struct Request {
    command: String,
}

#[derive(Deserialize, Serialize)]
struct Response {
    ok: bool,
    message: String,
}

pub struct ControlServer {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ControlServer {
    pub fn bind(
        endpoint: PathBuf,
        state: Arc<Mutex<FleetState>>,
        stop: Arc<AtomicBool>,
    ) -> Result<Self> {
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("devctl-control".into())
            .spawn(move || {
                if let Err(error) = serve(&endpoint, state, &thread_stop, ready_tx) {
                    eprintln!("devctl: control endpoint failed: {error:#}");
                    thread_stop.store(true, Ordering::SeqCst);
                }
            })
            .context("spawn control endpoint")?;
        match ready_rx.recv_timeout(BIND_DEADLINE) {
            Ok(Ok(())) => Ok(Self {
                stop,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                let _ = thread.join();
                bail!("control endpoint did not become ready within {BIND_DEADLINE:?}")
            }
        }
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn request(endpoint: &Path, command: &str, expected: &ProcessIdentity) -> Result<String> {
    request_raw(endpoint, command, expected)
}

fn response(bytes: &[u8], state: &Arc<Mutex<FleetState>>, stop: &AtomicBool) -> Vec<u8> {
    let response = match serde_json::from_slice::<Request>(bytes) {
        Ok(request) if request.command == "status" => {
            let state = state.lock().expect("state mutex poisoned");
            Response {
                ok: true,
                message: format!(
                    "{} {} ({}/{} healthy)",
                    state.topology(),
                    format!("{:?}", state.status()).to_lowercase(),
                    state
                        .processes()
                        .iter()
                        .filter(|p| matches!(p.status(), processctl::ManagedStatus::Healthy))
                        .count(),
                    state.processes().len()
                ),
            }
        }
        Ok(request) if request.command == "down" => {
            stop.store(true, Ordering::SeqCst);
            Response {
                ok: true,
                message: "shutdown requested".into(),
            }
        }
        Ok(_) => Response {
            ok: false,
            message: "unknown control command".into(),
        },
        Err(_) => Response {
            ok: false,
            message: "invalid control request".into(),
        },
    };
    serde_json::to_vec(&response).unwrap_or_default()
}

fn read_frame(stream: &mut impl Read, stop: &AtomicBool) -> Result<Vec<u8>> {
    let mut length = [0u8; 4];
    read_exact_bounded(stream, &mut length, stop)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME {
        bail!("control frame length {length} is outside 1..={MAX_FRAME}");
    }
    let mut bytes = vec![0; length];
    read_exact_bounded(stream, &mut bytes, stop)?;
    Ok(bytes)
}

fn write_frame(stream: &mut impl Write, bytes: &[u8], stop: &AtomicBool) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        bail!("control response exceeds frame bound");
    }
    let mut frame = Vec::with_capacity(4 + bytes.len());
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend_from_slice(bytes);
    write_all_bounded(stream, &frame, stop)
}

fn read_exact_bounded(
    stream: &mut impl Read,
    mut bytes: &mut [u8],
    stop: &AtomicBool,
) -> Result<()> {
    let deadline = Instant::now() + IO_DEADLINE;
    while !bytes.is_empty() {
        if stop.load(Ordering::SeqCst) {
            bail!("control I/O cancelled");
        }
        match stream.read(bytes) {
            Ok(0) if Instant::now() < deadline => std::thread::sleep(POLL),
            Ok(0) => bail!("control read deadline elapsed"),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if retryable(&error) && Instant::now() < deadline => {
                std::thread::sleep(POLL)
            }
            Err(error) if retryable(&error) => bail!("control read deadline elapsed"),
            Err(error) => return Err(error).context("read control frame"),
        }
        if Instant::now() >= deadline && !bytes.is_empty() {
            bail!("control read deadline elapsed");
        }
    }
    Ok(())
}

fn write_all_bounded(stream: &mut impl Write, mut bytes: &[u8], stop: &AtomicBool) -> Result<()> {
    let deadline = Instant::now() + IO_DEADLINE;
    while !bytes.is_empty() {
        if stop.load(Ordering::SeqCst) {
            bail!("control I/O cancelled");
        }
        match stream.write(bytes) {
            Ok(0) if Instant::now() < deadline => std::thread::sleep(POLL),
            Ok(0) => bail!("control write deadline elapsed"),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if retryable(&error) && Instant::now() < deadline => {
                std::thread::sleep(POLL)
            }
            Err(error) if retryable(&error) => bail!("control write deadline elapsed"),
            Err(error) => return Err(error).context("write control frame"),
        }
    }
    Ok(())
}

fn retryable(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        || error.kind() == std::io::ErrorKind::TimedOut
        || matches!(error.raw_os_error(), Some(232 | 536))
}

fn decode_response(bytes: &[u8]) -> Result<String> {
    let response: Response = serde_json::from_slice(bytes).context("decode control response")?;
    if response.ok {
        Ok(response.message)
    } else {
        bail!("{}", response.message)
    }
}

#[cfg(unix)]
fn serve(
    endpoint: &Path,
    state: Arc<Mutex<FleetState>>,
    stop: &AtomicBool,
    ready: std::sync::mpsc::SyncSender<Result<()>>,
) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixListener;
    let _ = std::fs::remove_file(endpoint);
    let listener = match UnixListener::bind(endpoint) {
        Ok(listener) => listener,
        Err(error) => {
            let message = error.to_string();
            let _ = ready.send(Err(error).context("bind Unix control socket"));
            bail!(message);
        }
    };
    std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let _ = ready.send(Ok(()));
    while !stop.load(Ordering::SeqCst) {
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(POLL);
                continue;
            }
            Err(error) => return Err(error).context("accept Unix control client"),
        };
        // The peer's uid is the authoritative gate on who may drive the endpoint;
        // the socket's 0o600 mode is belt-and-braces. Silently drop any client
        // that is not our own euid (or whose credentials we cannot read).
        match peer_credentials(stream.as_raw_fd()) {
            Ok((_pid, uid)) if uid == unsafe { libc::geteuid() } => {}
            _ => continue,
        }
        stream.set_nonblocking(true)?;
        if let Ok(request) = read_frame(&mut stream, stop) {
            let response_stop = AtomicBool::new(false);
            let _ = write_frame(
                &mut stream,
                &response(&request, &state, stop),
                &response_stop,
            );
        }
    }
    let _ = std::fs::remove_file(endpoint);
    Ok(())
}

#[cfg(unix)]
fn request_raw(endpoint: &Path, command: &str, expected: &ProcessIdentity) -> Result<String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;
    let metadata = std::fs::symlink_metadata(endpoint)?;
    if metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        bail!("control socket ownership or mode is invalid");
    }
    let observed = processctl::observe_process_identity(expected.pid)?;
    if &observed != expected {
        bail!("supervisor identity no longer matches state");
    }
    let mut stream = UnixStream::connect(endpoint)?;
    // Pin BOTH the peer pid (the anti-reused-pid guard) and the peer uid to the
    // recorded supervisor. Any failure to read credentials fails closed the same
    // way a mismatch does.
    let (pid, uid) = match peer_credentials(stream.as_raw_fd()) {
        Ok(credentials) => credentials,
        Err(_) => bail!("Unix control peer is not the recorded supervisor"),
    };
    if pid != expected.pid || uid != unsafe { libc::geteuid() } {
        bail!("Unix control peer is not the recorded supervisor");
    }
    stream.set_nonblocking(true)?;
    let never_stop = AtomicBool::new(false);
    write_frame(
        &mut stream,
        &serde_json::to_vec(&Request {
            command: command.into(),
        })?,
        &never_stop,
    )?;
    decode_response(&read_frame(&mut stream, &never_stop)?)
}

/// The single authority that decides peer identity on a Unix control socket:
/// it maps a connected socket fd to `(pid, uid)`. Both `serve` (uid gate) and
/// `request_raw` (pid + uid gate) route through here, so the comparison logic
/// lives in exactly one place per call site and never forks per platform — only
/// the OS credential-extraction syscall below differs.
#[cfg(target_os = "linux")]
fn peer_credentials(fd: std::os::unix::io::RawFd) -> Result<(u32, u32)> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("read SO_PEERCRED");
    }
    Ok((credentials.pid as u32, credentials.uid))
}

/// macOS does NOT bundle the peer pid into its credential struct the way Linux's
/// `ucred` does, so this takes TWO `getsockopt` calls at `SOL_LOCAL` (not
/// `SOL_SOCKET`): `LOCAL_PEERCRED` yields an `xucred` carrying the uid but no pid,
/// and `LOCAL_PEERPID` yields the pid on its own. A `cr_version` mismatch means the
/// kernel handed back a struct we cannot interpret, so the credentials are unusable.
#[cfg(target_os = "macos")]
fn peer_credentials(fd: std::os::unix::io::RawFd) -> Result<(u32, u32)> {
    let mut credentials: libc::xucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::xucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERCRED,
            (&raw mut credentials).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("read LOCAL_PEERCRED");
    }
    if credentials.cr_version != libc::XUCRED_VERSION {
        bail!("LOCAL_PEERCRED returned an unrecognized xucred version");
    }
    let mut pid: libc::pid_t = 0;
    let mut length = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("read LOCAL_PEERPID");
    }
    Ok((pid as u32, credentials.cr_uid))
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn peer_credentials(_: std::os::unix::io::RawFd) -> Result<(u32, u32)> {
    bail!("devctl has no peer-credential support for this Unix variant")
}

#[cfg(windows)]
fn serve(
    endpoint: &Path,
    state: Arc<Mutex<FleetState>>,
    stop: &AtomicBool,
    ready: std::sync::mpsc::SyncSender<Result<()>>,
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, ERROR_PIPE_LISTENING,
        INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_NOWAIT, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    };
    let sid = current_sid()?;
    let sddl: Vec<u16> = std::ffi::OsStr::new(&format!("O:{sid}D:P(A;;GA;;;{sid})"))
        .encode_wide()
        .chain(Some(0))
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
        let error = std::io::Error::last_os_error();
        let message = error.to_string();
        let _ = ready.send(Err(error).context("create pipe owner DACL"));
        bail!(message);
    }
    let name: Vec<u16> = endpoint.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut first = true;
    while !stop.load(Ordering::SeqCst) {
        let attributes = windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>()
                as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let pipe = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                4,
                MAX_FRAME as u32,
                MAX_FRAME as u32,
                0,
                &attributes,
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            let error = std::io::Error::last_os_error();
            if first {
                let message = error.to_string();
                let _ = ready.send(Err(error).context("create named pipe"));
                unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor as _) };
                bail!(message);
            }
            unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor as _) };
            return Err(error).context("create named pipe");
        }
        if first {
            first = false;
            let _ = ready.send(Ok(()));
        }
        loop {
            if stop.load(Ordering::SeqCst) {
                unsafe { CloseHandle(pipe) };
                break;
            }
            let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) } != 0;
            if connected
                || std::io::Error::last_os_error().raw_os_error()
                    == Some(ERROR_PIPE_CONNECTED as i32)
            {
                let mut file = unsafe { std::fs::File::from_raw_handle(pipe.cast()) };
                if let Ok(request) = read_frame(&mut file, stop) {
                    let response_stop = AtomicBool::new(false);
                    let _ =
                        write_frame(&mut file, &response(&request, &state, stop), &response_stop);
                }
                break;
            }
            let code = std::io::Error::last_os_error().raw_os_error();
            if code == Some(ERROR_PIPE_LISTENING as i32) {
                std::thread::sleep(POLL);
                continue;
            }
            unsafe { CloseHandle(pipe) };
            if code == Some(ERROR_NO_DATA as i32) {
                break;
            }
            unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor as _) };
            bail!("connect named pipe: {}", std::io::Error::last_os_error());
        }
    }
    unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor as _) };
    Ok(())
}

#[cfg(windows)]
fn request_raw(endpoint: &Path, command: &str, expected: &ProcessIdentity) -> Result<String> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};
    use windows_sys::Win32::System::Pipes::{
        GetNamedPipeServerProcessId, SetNamedPipeHandleState, WaitNamedPipeW, PIPE_NOWAIT,
        PIPE_READMODE_BYTE,
    };
    let name: Vec<u16> = endpoint.as_os_str().encode_wide().chain(Some(0)).collect();
    let deadline = Instant::now() + IO_DEADLINE;
    let handle = loop {
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            break handle;
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::last_os_error())
                .context("connect named pipe deadline elapsed");
        }
        unsafe { WaitNamedPipeW(name.as_ptr(), POLL.as_millis() as u32) };
    };
    let mut pid = 0;
    if unsafe { GetNamedPipeServerProcessId(handle, &mut pid) } == 0
        || pid != expected.pid
        || processctl::observe_process_identity(pid)? != *expected
    {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        bail!("named pipe server is not the recorded supervisor");
    }
    let mode = PIPE_READMODE_BYTE | PIPE_NOWAIT;
    if unsafe { SetNamedPipeHandleState(handle, &mode, std::ptr::null(), std::ptr::null()) } == 0 {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        return Err(std::io::Error::last_os_error()).context("make control client nonblocking");
    }
    let mut file = unsafe { std::fs::File::from_raw_handle(handle.cast()) };
    let never_stop = AtomicBool::new(false);
    write_frame(
        &mut file,
        &serde_json::to_vec(&Request {
            command: command.into(),
        })?,
        &never_stop,
    )?;
    decode_response(&read_frame(&mut file, &never_stop)?)
}

#[cfg(windows)]
fn current_sid() -> Result<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(std::io::Error::last_os_error()).context("open process token");
    }
    let mut required = 0;
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut required) };
    let mut buffer = vec![0usize; (required as usize).div_ceil(std::mem::size_of::<usize>())];
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
        unsafe { CloseHandle(token) };
        return Err(std::io::Error::last_os_error()).context("read process token");
    }
    unsafe { CloseHandle(token) };
    let user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
        return Err(std::io::Error::last_os_error()).context("format user SID");
    }
    let length = (0..).find(|&i| unsafe { *text.add(i) } == 0).unwrap_or(0);
    let result = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })?;
    unsafe { windows_sys::Win32::Foundation::LocalFree(text.cast()) };
    Ok(result)
}

#[cfg(not(any(windows, unix)))]
fn serve(
    _: &Path,
    _: Arc<Mutex<FleetState>>,
    _: &AtomicBool,
    ready: std::sync::mpsc::SyncSender<Result<()>>,
) -> Result<()> {
    let _ = ready.send(Err(anyhow::anyhow!(
        "devctl supports only Windows and Unix"
    )));
    bail!("devctl supports only Windows and Unix")
}
#[cfg(not(any(windows, unix)))]
fn request_raw(_: &Path, _: &str, _: &ProcessIdentity) -> Result<String> {
    bail!("devctl supports only Windows and Unix")
}
