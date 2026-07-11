#[cfg(not(windows))]
fn main() {
    eprintln!("winctrl is only supported on Windows");
    std::process::exit(2);
}

#[cfg(windows)]
mod windows {
    use std::ffi::{OsStr, OsString};
    use std::fs::{File, OpenOptions};
    use std::io::{self, Write};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use std::ptr::null;
    use windows_sys::Win32::Foundation::{
        CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
    };
    use windows_sys::Win32::System::Console::{
        AttachConsole, FreeConsole, GenerateConsoleCtrlEvent, GetStdHandle,
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, STD_INPUT_HANDLE,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, TerminateProcess, WaitForSingleObject, CREATE_NEW_PROCESS_GROUP,
        CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
    };

    const CHILD_CLEANUP_WAIT_MS: u32 = 5_000;

    pub fn main() -> Result<(), String> {
        let mut args = std::env::args_os();
        let _program = args.next();
        match args.next().as_deref() {
            Some(command) if command == "spawn" => spawn(args.collect()),
            Some(command) if command == "break" => send_break(args.collect()),
            _ => Err(usage()),
        }
    }

    fn usage() -> String {
        "usage: winctrl spawn --pid-file PATH --stdout PATH --stderr PATH -- EXE [ARGS...]\n       winctrl break PID".into()
    }

    fn spawn(args: Vec<OsString>) -> Result<(), String> {
        let mut pid_file = None;
        let mut stdout = None;
        let mut stderr = None;
        let mut i = 0;
        while i < args.len() {
            if args[i] == "--" {
                i += 1;
                break;
            }
            let target = match args[i].to_str() {
                Some("--pid-file") => &mut pid_file,
                Some("--stdout") => &mut stdout,
                Some("--stderr") => &mut stderr,
                _ => return Err(usage()),
            };
            i += 1;
            let value = args.get(i).ok_or_else(usage)?;
            *target = Some(PathBuf::from(value));
            i += 1;
        }
        let command = args.get(i..).filter(|v| !v.is_empty()).ok_or_else(usage)?;
        let pid_file = pid_file.ok_or_else(usage)?;
        let stdout_path = stdout.ok_or_else(usage)?;
        let stderr_path = stderr.ok_or_else(usage)?;

        let stdout_file = log_file(&stdout_path)?;
        let stderr_file = log_file(&stderr_path)?;
        let stdout_handle = stdout_file.as_raw_handle() as HANDLE;
        let stderr_handle = stderr_file.as_raw_handle() as HANDLE;
        unsafe {
            if SetHandleInformation(stdout_handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) == 0
                || SetHandleInformation(stderr_handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) == 0
            {
                return Err(last_error("make log handles inheritable"));
            }
        }

        let mut command_line = encode_command_line(command);
        let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        startup.dwFlags = STARTF_USESTDHANDLES;
        startup.hStdInput = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        startup.hStdOutput = stdout_handle;
        startup.hStdError = stderr_handle;
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let created = unsafe {
            CreateProcessW(
                null(), command_line.as_mut_ptr(), null(), null(), 1,
                CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT,
                null(), null(), &startup, &mut process,
            )
        };
        unsafe {
            let _ = SetHandleInformation(stdout_handle, HANDLE_FLAG_INHERIT, 0);
            let _ = SetHandleInformation(stderr_handle, HANDLE_FLAG_INHERIT, 0);
        }
        if created == 0 {
            return Err(last_error("CreateProcessW"));
        }
        #[cfg(test)]
        LAST_SPAWNED_PID.store(process.dwProcessId, std::sync::atomic::Ordering::SeqCst);
        if let Err(err) = write_pid_atomic(&pid_file, process.dwProcessId) {
            let cleanup = terminate_and_reap(&process);
            return Err(match cleanup {
                Ok(()) => format!("write pid file {}: {err}", pid_file.display()),
                Err(cleanup_err) => format!(
                    "write pid file {}: {err}; child cleanup failed: {cleanup_err}",
                    pid_file.display()
                ),
            });
        }
        unsafe {
            CloseHandle(process.hThread);
            CloseHandle(process.hProcess);
        }
        println!("{}", process.dwProcessId);
        Ok(())
    }

    fn terminate_and_reap(process: &PROCESS_INFORMATION) -> Result<(), String> {
        let result = unsafe {
            if TerminateProcess(process.hProcess, 1) == 0 {
                Err(last_error("TerminateProcess after PID publish failure"))
            } else {
                let wait = WaitForSingleObject(process.hProcess, CHILD_CLEANUP_WAIT_MS);
                if wait == windows_sys::Win32::Foundation::WAIT_OBJECT_0 {
                    Ok(())
                } else {
                    Err(format!("child did not exit within {CHILD_CLEANUP_WAIT_MS}ms (wait={wait:#x})"))
                }
            }
        };
        unsafe {
            CloseHandle(process.hThread);
            CloseHandle(process.hProcess);
        }
        result
    }

    fn send_break(args: Vec<OsString>) -> Result<(), String> {
        if args.len() != 1 { return Err(usage()); }
        let pid: u32 = args[0].to_string_lossy().parse().map_err(|_| usage())?;
        unsafe {
            let _ = FreeConsole();
            if AttachConsole(pid) == 0 { return Err(last_error("AttachConsole")); }
            if SetConsoleCtrlHandler(None, 1) == 0 {
                let err = last_error("SetConsoleCtrlHandler");
                let _ = FreeConsole();
                return Err(err);
            }
            if GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) == 0 {
                let err = last_error("GenerateConsoleCtrlEvent");
                let _ = FreeConsole();
                return Err(err);
            }
            if FreeConsole() == 0 { return Err(last_error("FreeConsole")); }
        }
        Ok(())
    }

    fn log_file(path: &Path) -> Result<File, String> {
        OpenOptions::new().create(true).truncate(true).write(true).open(path)
            .map_err(|e| format!("open {}: {e}", path.display()))
    }

    fn write_pid_atomic(path: &Path, pid: u32) -> io::Result<()> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let name = path.file_name().unwrap_or_else(|| OsStr::new("child.pid"));
        let temp = parent.join(format!(".{}.{}.tmp", name.to_string_lossy(), std::process::id()));
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temp)?;
        writeln!(file, "{pid}")?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(temp, path)
    }

    fn encode_command_line(args: &[OsString]) -> Vec<u16> {
        let mut line = OsString::new();
        for (index, arg) in args.iter().enumerate() {
            if index != 0 { line.push(" "); }
            line.push(quote_arg(arg));
        }
        line.encode_wide().chain(Some(0)).collect()
    }

    fn quote_arg(arg: &OsStr) -> OsString {
        let text = arg.to_string_lossy();
        if !text.is_empty() && !text.chars().any(|c| c == ' ' || c == '\t' || c == '"') {
            return arg.to_owned();
        }
        let mut out = String::from("\"");
        let mut slashes = 0;
        for ch in text.chars() {
            if ch == '\\' { slashes += 1; continue; }
            if ch == '"' { out.push_str(&"\\".repeat(slashes * 2 + 1)); out.push('"'); }
            else { out.push_str(&"\\".repeat(slashes)); out.push(ch); }
            slashes = 0;
        }
        out.push_str(&"\\".repeat(slashes * 2));
        out.push('"');
        OsString::from(out)
    }

    fn last_error(action: &str) -> String {
        format!("{action}: {}", io::Error::last_os_error())
    }

    #[cfg(test)]
    static LAST_SPAWNED_PID: std::sync::atomic::AtomicU32 =
        std::sync::atomic::AtomicU32::new(0);

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        const STILL_ACTIVE: u32 = 259;

        #[test]
        fn pid_publish_failure_terminates_spawned_child() {
            let root = std::env::temp_dir().join(format!(
                "winctrl-orphan-regression-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&root).unwrap();
            // `rename(temp, directory)` is invalid, after CreateProcessW has succeeded.
            let invalid_pid_target = root.join("pid-target-is-a-directory");
            std::fs::create_dir(&invalid_pid_target).unwrap();
            let args = vec![
                "--pid-file".into(), invalid_pid_target.into_os_string(),
                "--stdout".into(), root.join("out.log").into_os_string(),
                "--stderr".into(), root.join("err.log").into_os_string(),
                "--".into(), "powershell.exe".into(), "-NoProfile".into(),
                "-Command".into(), "Start-Sleep -Seconds 300".into(),
            ];
            let error = spawn(args).expect_err("PID publication must fail");
            assert!(error.contains("write pid file"), "{error}");

            let pid = LAST_SPAWNED_PID.load(std::sync::atomic::Ordering::SeqCst);
            assert_ne!(pid, 0, "CreateProcessW must have succeeded before publication failed");
            unsafe {
                let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                if !handle.is_null() {
                    let mut exit_code = 0;
                    assert_ne!(GetExitCodeProcess(handle, &mut exit_code), 0);
                    CloseHandle(handle);
                    assert_ne!(exit_code, STILL_ACTIVE, "spawned child {pid} survived PID failure");
                }
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}

#[cfg(windows)]
fn main() {
    if let Err(err) = windows::main() {
        eprintln!("winctrl: {err}");
        std::process::exit(1);
    }
}
