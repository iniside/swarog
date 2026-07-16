//! Bounded local control endpoint for `weles status` / `weles down`: a client
//! and a running supervisor talking over a loopback IPC channel (Windows named
//! pipe / Unix domain socket), length-prefixed JSON frames, every read/write
//! bounded by a deadline so a stuck peer can never hang either side.
//!
//! Zero-sharing: the protocol, frame codec, and both peer-identity validations
//! are COPIED (never imported) from `tools/devctl/src/control.rs`, adapted to
//! weles's own [`crate::state`] types. Differences from devctl:
//!
//! * weles has no `observe_process_identity` (no start-time observation), so
//!   the pre-connect liveness check is a plain pid probe ([`supervisor_alive`])
//!   plus a `started_unix` sanity check ([`classify`]); the connect-time
//!   guard against a reused pid answering is the transport peer check
//!   (Windows: `GetNamedPipeServerProcessId`; Unix: `SO_PEERCRED` pid+uid).
//! * `status` renders a per-service table from the CURRENT in-memory snapshot
//!   the supervisor publishes into the shared [`FleetState`] after each
//!   checkpoint — never a re-read of the file.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::state::{self, FleetState, FleetStatus, ProcessIdentity, Readiness, Status};

/// Upper bound on a single control frame (header + body). Copied intent from
/// devctl; sized generously (64 KiB) — a status table is far smaller.
const MAX_FRAME: usize = 64 * 1024;
/// Per read/write deadline: a peer that stalls mid-frame is abandoned.
const IO_DEADLINE: Duration = Duration::from_secs(2);
/// How long [`ControlServer::bind`] waits for the serve thread to report the
/// endpoint is listening before failing loudly.
const BIND_DEADLINE: Duration = Duration::from_secs(2);
/// Poll granularity for the non-blocking accept / bounded I/O loops.
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

/// What a `weles status`/`down` client should do given the loaded state, the
/// current wall-clock second, and whether the recorded supervisor is alive.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Disposition {
    /// Supervisor is live and the fleet is non-terminal: connect to the
    /// control endpoint.
    Connect,
    /// The fleet already finished (terminal) or never ran: print and exit 0.
    Inactive(String),
    /// State claims an active fleet but the supervisor is gone or its identity
    /// is implausible: stale file, the fleet is not up (error, non-zero exit).
    Stale(String),
}

/// Owns the control-endpoint serve thread; dropping it stops and joins it.
///
/// Stop-authority invariant (single ownership of fleet-stop): the ONLY code
/// that ever stores into the supervisor's fleet-stop atomic is a received
/// `down` request ([`response`]). The server's own lifecycle (bind timeout,
/// teardown via `Drop`, a dead serve thread) flows through the PRIVATE
/// `shutdown` atomic — a control-plane failure must never look like an
/// operator `down` and tear a healthy fleet down.
pub struct ControlServer {
    /// Private serve-loop/teardown flag — never the fleet stop.
    shutdown: Arc<AtomicBool>,
    /// Set (only) when the serve thread died irrecoverably: the endpoint is
    /// gone but the fleet keeps running.
    dead: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ControlServer {
    /// Binds the control endpoint and spawns the serve thread. Blocks until
    /// the thread reports the endpoint is actually listening (or fails within
    /// [`BIND_DEADLINE`]). `state` is the shared snapshot a `status` reply
    /// renders; `fleet_stop` is the supervisor's stop atomic — stored to ONLY
    /// when a `down` request arrives, never by any failure path in here.
    pub fn bind(
        endpoint: PathBuf,
        state: Arc<Mutex<FleetState>>,
        fleet_stop: Arc<AtomicBool>,
    ) -> Result<Self> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let dead = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let thread_shutdown = Arc::clone(&shutdown);
        let thread_dead = Arc::clone(&dead);
        let thread = std::thread::Builder::new()
            .name("weles-control".into())
            .spawn(move || {
                if let Err(error) = serve(&endpoint, state, &thread_shutdown, &fleet_stop, ready_tx)
                {
                    // Irrecoverable control-plane death: report it and flag it,
                    // but do NOT stop the fleet — a lost status/down endpoint
                    // is a degradation, never a phantom operator-down.
                    eprintln!(
                        "weles: control endpoint died: {error:#} — `weles status`/`down` are \
                         unavailable for this run; stop the fleet with Ctrl-C"
                    );
                    thread_dead.store(true, Ordering::SeqCst);
                }
            })
            .context("spawn control endpoint")?;
        match ready_rx.recv_timeout(BIND_DEADLINE) {
            Ok(Ok(())) => Ok(Self {
                shutdown,
                dead,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                shutdown.store(true, Ordering::SeqCst);
                let _ = thread.join();
                bail!("control endpoint did not become ready within {BIND_DEADLINE:?}")
            }
        }
    }

    /// Whether the serve thread died irrecoverably (endpoint gone, fleet
    /// unaffected). The supervisor reports this once, loudly.
    pub fn dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Connects to `endpoint`, validates the peer is the recorded supervisor, then
/// sends `command` and returns the reply message (or the failure).
pub fn request(endpoint: &Path, command: &str, expected: &ProcessIdentity) -> Result<String> {
    request_raw(endpoint, command, expected)
}

/// Client-side disposition: what to do with a loaded state file. Terminal ⇒
/// inactive; non-terminal but dead/implausible supervisor ⇒ stale; otherwise
/// connect. Pure over its inputs — the caller supplies `alive` (from
/// [`supervisor_alive`]) so this is unit-testable without probing a process.
pub fn classify(state: &FleetState, now_unix: u64, alive: bool) -> Disposition {
    if state.status.is_terminal() {
        return Disposition::Inactive(format!(
            "weles: inactive (last {} run {})",
            state.topology,
            format!("{:?}", state.status).to_lowercase()
        ));
    }
    if !alive || !identity_plausible(&state.supervisor, now_unix) {
        return Disposition::Stale(format!(
            "weles: stale state — {} run recorded {} but its supervisor (pid {}) is not \
             running; the fleet is not up",
            state.topology,
            format!("{:?}", state.status).to_lowercase(),
            state.supervisor.pid
        ));
    }
    Disposition::Connect
}

/// Sanity on the recorded supervisor identity: a start time in the (plausible)
/// past, never the future — a future timestamp means a corrupt or rewritten
/// state file, not a live supervisor. A small skew is tolerated for jitter.
fn identity_plausible(identity: &ProcessIdentity, now_unix: u64) -> bool {
    const SKEW: u64 = 5;
    identity.started_unix <= now_unix.saturating_add(SKEW)
}

/// Current wall-clock second (Unix epoch), for the `now_unix` argument of
/// [`classify`]. `0` if the clock is before the epoch (never, in practice).
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Polls `state_path` until the fleet reaches a terminal status (the supervisor
/// has finished teardown) or `timeout` elapses. Reports progress; a supervisor
/// that exits WITHOUT publishing a terminal state is an error (with a one-shot
/// re-read to defeat the write-then-exit race). Used by `weles down`.
pub fn wait_for_terminal(
    state_path: &Path,
    supervisor: &ProcessIdentity,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let state = load_required(state_path)?;
        if let Some(outcome) = terminal_outcome(&state) {
            return outcome;
        }
        if !supervisor_alive(supervisor) {
            // Race guard: the supervisor may have written the terminal state
            // and exited between our read and this check — re-read once before
            // declaring a premature exit.
            let state = load_required(state_path)?;
            if let Some(outcome) = terminal_outcome(&state) {
                return outcome;
            }
            bail!("weles: supervisor exited before publishing a terminal shutdown state");
        }
        if Instant::now() >= deadline {
            bail!("weles: timed out waiting {timeout:?} for the fleet to stop");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn load_required(state_path: &Path) -> Result<FleetState> {
    state::load(state_path)?.context("weles state file disappeared during shutdown")
}

fn terminal_outcome(state: &FleetState) -> Option<Result<()>> {
    match state.status {
        FleetStatus::Stopped => {
            println!(
                "weles: {} stopped ({} services reaped)",
                state.topology,
                state.services.len()
            );
            Some(Ok(()))
        }
        FleetStatus::Failed => Some(Err(anyhow::anyhow!(
            "weles: {} shutdown ended in a failed state",
            state.topology
        ))),
        FleetStatus::Starting | FleetStatus::Running | FleetStatus::Stopping => None,
    }
}

/// Renders a `status` reply: a header line plus one line per service, all from
/// the supervisor's last-published in-memory snapshot.
fn render_status(state: &FleetState) -> String {
    let healthy = state
        .services
        .iter()
        .filter(|svc| svc.status == Status::Healthy)
        .count();
    let mut out = format!(
        "weles {} — {} ({}/{} healthy)",
        state.topology,
        format!("{:?}", state.status).to_lowercase(),
        healthy,
        state.services.len()
    );
    for svc in &state.services {
        let pid = svc
            .pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "-".to_string());
        // Annotate a Healthy service with its post-healthy `/readyz` freshness;
        // omit it for non-Healthy services (readiness is Unknown there).
        let mut status = format!("{:?}", svc.status).to_lowercase();
        if svc.status == Status::Healthy {
            match svc.readiness {
                Readiness::Ready => status.push_str(" [ready]"),
                Readiness::Degraded => status.push_str(" [degraded]"),
                Readiness::Unreachable => status.push_str(" [unreachable]"),
                Readiness::Unknown => {}
            }
        }
        let _ = write!(
            out,
            "\n  {:<16} {:<24} pid {:<8} restarts {}",
            svc.name, status, pid, svc.restarts
        );
    }
    out
}

/// Builds the reply bytes for a received request frame. `status` renders the
/// shared snapshot; `down` sets the supervisor's fleet-stop atomic then
/// acknowledges — the ONE place in this module allowed to store into it.
fn response(bytes: &[u8], state: &Arc<Mutex<FleetState>>, fleet_stop: &AtomicBool) -> Vec<u8> {
    let response = match serde_json::from_slice::<Request>(bytes) {
        Ok(request) if request.command == "status" => {
            let state = state.lock().expect("state mutex poisoned");
            Response {
                ok: true,
                message: render_status(&state),
            }
        }
        Ok(request) if request.command == "down" => {
            fleet_stop.store(true, Ordering::SeqCst);
            Response {
                ok: true,
                message: "weles: shutdown requested".into(),
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

// ---------------------------------------------------------------------------
// Length-prefixed frame codec (bounded reads/writes, copied from devctl)
// ---------------------------------------------------------------------------

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
        // ERROR_NO_DATA (232) / ERROR_PIPE_NOT_CONNECTED (536): a nonblocking
        // pipe momentarily with no data / mid-handshake — retry, don't fail.
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

// ---------------------------------------------------------------------------
// Supervisor liveness probe (pre-connect stale-state detection)
// ---------------------------------------------------------------------------

/// Best-effort liveness probe for the recorded supervisor pid. A dead
/// supervisor beside a non-terminal state file means the state is stale. This
/// is only the PRE-connect check; the transport peer-identity check at connect
/// time is what actually guards against a reused pid answering.
///
/// Windows adds an ASYMMETRIC process-creation-time check to defeat pid reuse:
/// see [`supervisor_alive`]'s body. Linux/other stay pid-only — a documented
/// known gap: `/proc/<pid>/stat` starttime is in clock ticks since boot, which
/// needs `/proc/stat btime` + `sysconf(_SC_CLK_TCK)` epoch arithmetic with no
/// prior art in this repo to copy, so it is deliberately deferred. The failure
/// mode of pid-only liveness is benign OVER-protection (a stale file's reused
/// pid is treated as live), and the connect-time peer check still rejects a
/// wrong peer answering the endpoint; only the pre-connect classification is
/// slightly conservative there.
///
/// Windows: after opening the process (its
/// `PROCESS_QUERY_LIMITED_INFORMATION` handle already suffices for
/// `GetProcessTimes`), fetch the creation time and reject the pid as reused
/// ONLY when the process was created strictly LATER than the recorded start.
/// The asymmetry is the whole point — never false-declare a LIVE supervisor
/// dead: `identity.started_unix` is `SystemTime::now()` captured INSIDE
/// `run_up`, strictly AFTER the OS created the process, with an unbounded gap
/// (AV scan, loaded box, debug build) plus ~1s `as_secs()` truncation. So a
/// real live supervisor always has `actual_creation_unix <= started_unix`; a
/// reused pid belongs to a process created LATER, so
/// `actual_creation_unix > started_unix`. A symmetric `|Δ| <= TOL` check would
/// false-kill a live slow-start supervisor — flipping retention from benign
/// over-protection to DELETING THE LIVE GENERATION — so we reject only the
/// strictly-later side. See [`is_reused_pid`].
#[cfg(windows)]
pub fn supervisor_alive(identity: &ProcessIdentity) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
    // A few seconds of skew absorbs the `as_secs()` truncation on both the
    // recorded start and the computed creation second; distinct in MEANING from
    // `identity_plausible`'s SKEW (that one bounds a FUTURE recorded start vs
    // wall-clock — this one bounds actual-creation vs recorded-start).
    const CREATION_SKEW: u64 = 5;
    // SAFETY: probing an arbitrary pid; a null handle (gone / access denied) is
    // treated as dead, and the opened handle is closed before returning.
    unsafe {
        let handle = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE_ACCESS,
            0,
            identity.pid,
        );
        if handle.is_null() {
            return false;
        }
        let pid_alive = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
        // Combine the creation FILETIME's Hi/Lo halves into 100ns ticks, exactly
        // as `tools/processctl/src/platform/windows.rs:549` does (copied, not
        // imported — zero-sharing). If GetProcessTimes fails we cannot judge
        // reuse, so we DON'T reject (benign over-protection over false-kill).
        let mut created: FILETIME = std::mem::zeroed();
        let mut exited: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        let reused = if GetProcessTimes(
            handle,
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        ) != 0
        {
            let ticks =
                (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
            is_reused_pid(
                identity.started_unix,
                filetime_to_unix(ticks),
                CREATION_SKEW,
            )
        } else {
            false
        };
        CloseHandle(handle);
        pid_alive && !reused
    }
}

/// Convert a Windows creation FILETIME (100ns ticks since 1601-01-01 UTC,
/// already combined as `Hi << 32 | Lo`) to Unix seconds. The 1601→1970 epoch
/// offset is 11_644_473_600 seconds; a pre-epoch value saturates to 0.
#[cfg(windows)]
fn filetime_to_unix(ticks: u64) -> u64 {
    const EPOCH_OFFSET_SECS: u64 = 11_644_473_600;
    (ticks / 10_000_000).saturating_sub(EPOCH_OFFSET_SECS)
}

/// Asymmetric pid-reuse test. A process behind a recorded pid is a REUSE only
/// when its actual creation time is strictly LATER than the recorded start
/// (plus `skew`). This one-sided comparison is deliberate: the recorded
/// `started_unix` is captured AFTER the OS created the process, so a live
/// supervisor always satisfies `actual <= recorded` and is NEVER reported
/// reused — no matter how slow its start was. Saturating add so a huge recorded
/// start can't wrap.
#[cfg(windows)]
fn is_reused_pid(recorded_started_unix: u64, actual_creation_unix: u64, skew: u64) -> bool {
    actual_creation_unix > recorded_started_unix.saturating_add(skew)
}

#[cfg(unix)]
pub fn supervisor_alive(identity: &ProcessIdentity) -> bool {
    // SAFETY: signal 0 only checks existence/permission, sends nothing.
    // Pid-only (no start-time asymmetry) — documented known gap on the fn above.
    unsafe { libc::kill(identity.pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(any(unix, windows)))]
pub fn supervisor_alive(_: &ProcessIdentity) -> bool {
    false
}

// ---------------------------------------------------------------------------
// Unix domain socket transport (Linux: SO_PEERCRED peer validation)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn serve(
    endpoint: &Path,
    state: Arc<Mutex<FleetState>>,
    shutdown: &AtomicBool,
    fleet_stop: &AtomicBool,
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
    while !shutdown.load(Ordering::SeqCst) {
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(POLL);
                continue;
            }
            Err(error) => {
                // A per-accept hiccup must never kill the endpoint (let alone
                // the fleet): report and keep accepting.
                eprintln!("weles: control accept failed ({error:#}); continuing");
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        // SO_PEERCRED: only our own euid may drive the endpoint (mode 0600 also
        // enforces this, but the credential check is the authoritative gate).
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
        if stream.set_nonblocking(true).is_err() {
            continue; // per-connection failure: drop this client, keep serving
        }
        if let Ok(request) = read_frame(&mut stream, shutdown) {
            // A fresh always-false flag so writing the reply is never cancelled
            // by a `down` request having just set the fleet stop.
            let response_stop = AtomicBool::new(false);
            let _ = write_frame(
                &mut stream,
                &response(&request, &state, fleet_stop),
                &response_stop,
            );
        }
    }
    let _ = std::fs::remove_file(endpoint);
    Ok(())
}

// The client-side (`request_raw`) never touches any stop atomic — its bounded
// I/O uses a local always-false flag.
#[cfg(target_os = "linux")]
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
    let mut stream = UnixStream::connect(endpoint)?;
    // Pin the peer's pid AND uid against the recorded supervisor identity: a
    // reused socket path answered by anyone but the supervisor is rejected.
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
        || credentials.pid as u32 != expected.pid
        || credentials.uid != unsafe { libc::geteuid() }
    {
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

// ---------------------------------------------------------------------------
// Windows named-pipe transport (owner-only DACL + server-pid peer validation)
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn serve(
    endpoint: &Path,
    state: Arc<Mutex<FleetState>>,
    shutdown: &AtomicBool,
    fleet_stop: &AtomicBool,
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
    // Owner-only, protected DACL (same SDDL technique as `lock.rs`): only the
    // creating user may open the pipe.
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
    while !shutdown.load(Ordering::SeqCst) {
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
                // The FIRST instance failing means the endpoint never came up:
                // fail the bind loudly (the caller decides what that means).
                unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor as _) };
                let message = error.to_string();
                let _ = ready.send(Err(error).context("create named pipe"));
                bail!(message);
            }
            // A MID-RUN re-create failure is a control-plane hiccup: report and
            // retry — never kill the endpoint (let alone the fleet) over it.
            eprintln!("weles: recreate control pipe failed ({error}); retrying");
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        if first {
            first = false;
            let _ = ready.send(Ok(()));
        }
        loop {
            if shutdown.load(Ordering::SeqCst) {
                unsafe { CloseHandle(pipe) };
                break;
            }
            let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) } != 0;
            if connected
                || std::io::Error::last_os_error().raw_os_error()
                    == Some(ERROR_PIPE_CONNECTED as i32)
            {
                let mut file = unsafe { std::fs::File::from_raw_handle(pipe.cast()) };
                if let Ok(request) = read_frame(&mut file, shutdown) {
                    let response_stop = AtomicBool::new(false);
                    let _ = write_frame(
                        &mut file,
                        &response(&request, &state, fleet_stop),
                        &response_stop,
                    );
                }
                break;
            }
            let code = std::io::Error::last_os_error().raw_os_error();
            if code == Some(ERROR_PIPE_LISTENING as i32) {
                std::thread::sleep(POLL);
                continue;
            }
            // Capture the error INTO the message from the already-saved `code`
            // BEFORE CloseHandle — the syscalls in cleanup overwrite
            // last_os_error(), so re-fetching after them reports the wrong
            // error (devctl's control.rs has exactly that bug; deliberately
            // not copied). An unexpected connect error is per-connection:
            // report, drop this instance, recreate — never kill the endpoint.
            if code != Some(ERROR_NO_DATA as i32) {
                eprintln!(
                    "weles: connect control pipe failed (os error {code:?}); \
                     recreating the pipe instance"
                );
            }
            unsafe { CloseHandle(pipe) };
            break;
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
        // The server recreates the pipe instance between serial connections;
        // wait for the next instance rather than failing on the brief gap.
        unsafe { WaitNamedPipeW(name.as_ptr(), POLL.as_millis() as u32) };
    };
    // The pipe server process must BE the recorded supervisor (guards against
    // a stale pipe name answered by an unrelated process with a reused pid).
    let mut pid = 0;
    if unsafe { GetNamedPipeServerProcessId(handle, &mut pid) } == 0 || pid != expected.pid {
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

/// The current user's SID as an SDDL string, for the owner-only pipe DACL.
/// Copied from `tools/devctl/src/control.rs::current_sid` (weles's `lock.rs`
/// has an equivalent but non-exported helper).
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

// ---------------------------------------------------------------------------
// Unsupported-target fallbacks (weles targets Windows + Linux, like devctl)
// ---------------------------------------------------------------------------

#[cfg(not(any(windows, target_os = "linux")))]
fn serve(
    _: &Path,
    _: Arc<Mutex<FleetState>>,
    _: &AtomicBool,
    ready: std::sync::mpsc::SyncSender<Result<()>>,
) -> Result<()> {
    let _ = ready.send(Err(anyhow::anyhow!(
        "weles control endpoint supports only Windows and Linux"
    )));
    bail!("weles control endpoint supports only Windows and Linux")
}

#[cfg(not(any(windows, target_os = "linux")))]
fn request_raw(_: &Path, _: &str, _: &ProcessIdentity) -> Result<String> {
    bail!("weles control endpoint supports only Windows and Linux")
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod control_tests;
