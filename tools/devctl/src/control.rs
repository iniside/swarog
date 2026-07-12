use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use processctl::{FleetState, ProcessIdentity};
use serde::{Deserialize, Serialize};

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
    endpoint: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ControlServer {
    pub fn bind(
        endpoint: PathBuf,
        state: Arc<Mutex<FleetState>>,
        stop: Arc<AtomicBool>,
    ) -> Result<Self> {
        let thread_stop = Arc::clone(&stop);
        let thread_endpoint = endpoint.clone();
        let thread = std::thread::Builder::new()
            .name("devctl-control".into())
            .spawn(move || {
                if let Err(error) = serve(&thread_endpoint, state, &thread_stop) {
                    eprintln!("devctl: control endpoint failed: {error:#}");
                    thread_stop.store(true, Ordering::SeqCst);
                }
            })
            .context("spawn control endpoint")?;
        Ok(Self {
            endpoint,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = request_raw(&self.endpoint, "wake", None);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        #[cfg(target_os = "linux")]
        let _ = std::fs::remove_file(&self.endpoint);
    }
}

pub fn request(endpoint: &Path, command: &str, expected: &ProcessIdentity) -> Result<String> {
    request_raw(endpoint, command, Some(expected))
}

fn handle(bytes: &[u8], state: &Arc<Mutex<FleetState>>, stop: &AtomicBool) -> Vec<u8> {
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
                        .filter(|process| matches!(
                            process.status(),
                            processctl::ManagedStatus::Healthy
                        ))
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
        Ok(request) if request.command == "wake" => Response {
            ok: true,
            message: "wake".into(),
        },
        Ok(_) => Response {
            ok: false,
            message: "unknown control command".into(),
        },
        Err(_) => Response {
            ok: false,
            message: "invalid control request".into(),
        },
    };
    serde_json::to_vec(&response)
        .unwrap_or_else(|_| b"{\"ok\":false,\"message\":\"encode failure\"}".to_vec())
}

#[cfg(target_os = "linux")]
fn serve(endpoint: &Path, state: Arc<Mutex<FleetState>>, stop: &AtomicBool) -> Result<()> {
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::io::AsRawFd as _;
    use std::os::unix::net::UnixListener;

    let _ = std::fs::remove_file(endpoint);
    let listener = UnixListener::bind(endpoint).context("bind Unix control socket")?;
    std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600))
        .context("restrict Unix control socket")?;
    for stream in listener.incoming() {
        let mut stream = stream.context("accept Unix control client")?;
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut credentials).cast(),
                &mut length,
            )
        } != 0
            || credentials.uid != unsafe { libc::geteuid() }
        {
            continue;
        }
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes)?;
        stream.write_all(&handle(&bytes, &state, stop))?;
        if stop.load(Ordering::SeqCst) {
            break;
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn request_raw(
    endpoint: &Path,
    command: &str,
    expected: Option<&ProcessIdentity>,
) -> Result<String> {
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::os::unix::net::UnixStream;
    let metadata = std::fs::symlink_metadata(endpoint).context("inspect Unix control socket")?;
    if metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        bail!("control socket ownership or mode is invalid");
    }
    if let Some(expected) = expected {
        let observed = processctl::observe_process_identity(expected.pid)?;
        if &observed != expected {
            bail!("supervisor identity no longer matches state");
        }
    }
    let mut stream = UnixStream::connect(endpoint).context("connect Unix control socket")?;
    stream.write_all(&serde_json::to_vec(&Request {
        command: command.into(),
    })?)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes)?;
    decode_response(&bytes)
}

#[cfg(windows)]
fn serve(endpoint: &Path, state: Arc<Mutex<FleetState>>, stop: &AtomicBool) -> Result<()> {
    use std::io::{Read as _, Write as _};
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::FromRawHandle as _;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    let sid = current_sid()?;
    let sddl: Vec<u16> = std::ffi::OsStr::new(&format!("O:{sid}D:P(A;;GA;;;{sid})"))
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
        return Err(std::io::Error::last_os_error()).context("create pipe owner DACL");
    }
    let name: Vec<u16> = endpoint
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    loop {
        let mut attributes = windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>()
                as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let pipe = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                4096,
                4096,
                0,
                &mut attributes,
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor as _) };
            return Err(std::io::Error::last_os_error()).context("create named pipe");
        }
        let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) } != 0
            || std::io::Error::last_os_error().raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32);
        if !connected {
            unsafe { CloseHandle(pipe) };
            continue;
        }
        let mut file = unsafe { std::fs::File::from_raw_handle(pipe.cast()) };
        let mut bytes = [0u8; 4096];
        let read = file.read(&mut bytes)?;
        file.write_all(&handle(&bytes[..read], &state, stop))?;
        if stop.load(Ordering::SeqCst) {
            break;
        }
    }
    unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor as _) };
    Ok(())
}

#[cfg(windows)]
fn request_raw(
    endpoint: &Path,
    command: &str,
    expected: Option<&ProcessIdentity>,
) -> Result<String> {
    use std::io::{Read as _, Write as _};
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::FromRawHandle as _;
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
    let name: Vec<u16> = endpoint
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
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
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error()).context("connect named pipe");
    }
    let mut pid = 0;
    if unsafe { GetNamedPipeServerProcessId(handle, &mut pid) } == 0 {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        return Err(std::io::Error::last_os_error()).context("identify named pipe server");
    }
    if let Some(expected) = expected {
        let observed = processctl::observe_process_identity(pid)?;
        if pid != expected.pid || &observed != expected {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            bail!("named pipe server is not the recorded supervisor");
        }
    }
    let mut file = unsafe { std::fs::File::from_raw_handle(handle.cast()) };
    file.write_all(&serde_json::to_vec(&Request {
        command: command.into(),
    })?)?;
    file.flush()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    decode_response(&bytes)
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

#[cfg(not(any(windows, target_os = "linux")))]
fn serve(_: &Path, _: Arc<Mutex<FleetState>>, _: &AtomicBool) -> Result<()> {
    bail!("devctl supports only Windows and Linux")
}
#[cfg(not(any(windows, target_os = "linux")))]
fn request_raw(_: &Path, _: &str, _: Option<&ProcessIdentity>) -> Result<String> {
    bail!("devctl supports only Windows and Linux")
}

fn decode_response(bytes: &[u8]) -> Result<String> {
    let response: Response = serde_json::from_slice(bytes).context("decode control response")?;
    if response.ok {
        Ok(response.message)
    } else {
        bail!("{}", response.message)
    }
}
