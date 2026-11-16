//! Daemon lifecycle: starting, finding and stopping the background server.
//!
//! The daemon is self-contained: a pid file, a health probe and a signal. No
//! launchd, no systemd, no root, and the same behaviour on macOS and Linux.
//!
//! The pid file holds three lines — the process id, the address the daemon
//! bound, and the port it is actually serving. Those last two are what let
//! `eggpass status`, `eggpass port` and `eggpass host` report the address
//! that is live *right now*, which may differ from the configured one after a
//! hand edit of the config file.
//!
//! A pid file written by an older build has only two lines — the process id
//! and the port — and still parses.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(unix)]
use nix::sys::signal::{Signal, kill};
#[cfg(unix)]
use nix::unistd::Pid;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use std::process::{Command, Stdio};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, GetLastError, STILL_ACTIVE,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE, TerminateProcess,
};

/// How long `stop` waits for the daemon to exit after requesting it to stop.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// How long `run` waits for the daemon to answer `/health`.
pub const START_TIMEOUT: Duration = Duration::from_secs(10);

/// What we know about a daemon that may or may not be running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonState {
    /// The daemon is alive and answering.
    Running {
        /// The process id.
        pid: i32,
        /// The address it bound.
        host: String,
        /// The port it is serving.
        port: u16,
    },
    /// Something is recorded, but it is not a live daemon.
    Stale {
        /// The recorded process id.
        pid: i32,
    },
    /// No daemon.
    Stopped,
}

impl DaemonState {
    /// Whether the daemon is running.
    pub fn is_running(&self) -> bool {
        matches!(self, DaemonState::Running { .. })
    }
}

/// Errors raised while managing the daemon.
#[derive(Debug)]
pub enum DaemonError {
    /// A file could not be read or written.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The daemon could not be forked.
    Daemonize(String),
    /// A signal could not be sent.
    Signal(String),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonError::Io { path, source } => write!(f, "{}: {source}", path.display()),
            DaemonError::Daemonize(message) => write!(f, "could not start the daemon: {message}"),
            DaemonError::Signal(message) => write!(f, "could not signal the daemon: {message}"),
        }
    }
}

impl std::error::Error for DaemonError {}

/// The contents of the pid file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonInfo {
    /// The process id.
    pub pid: i32,
    /// The address the daemon bound, when it is recorded.
    pub host: Option<String>,
    /// The port the daemon is serving, when it is recorded.
    pub port: Option<u16>,
}

/// Read the pid file.
///
/// A two-line file is the older layout — the process id and the port — and is
/// still understood.
pub fn read_info(pid_file: &Path) -> Option<DaemonInfo> {
    let text = std::fs::read_to_string(pid_file).ok()?;
    let mut lines = text.lines();
    let pid: i32 = lines.next()?.trim().parse().ok()?;

    let second = lines.next().map(str::trim);
    let third = lines.next().map(str::trim);

    match second {
        // The older layout: the second line is the port.
        Some(second) if second.parse::<u16>().is_ok() => Some(DaemonInfo {
            pid,
            host: None,
            port: second.parse::<u16>().ok(),
        }),
        // The current layout: the second line is the host, the third the port.
        Some(second) => Some(DaemonInfo {
            pid,
            host: Some(second.to_string()),
            port: third.and_then(|line| line.parse::<u16>().ok()),
        }),
        None => Some(DaemonInfo {
            pid,
            host: None,
            port: None,
        }),
    }
}

/// Write the pid file.
pub fn write_info(pid_file: &Path, pid: i32, host: &str, port: u16) -> Result<(), DaemonError> {
    if let Some(parent) = pid_file.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent).map_err(|source| DaemonError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(pid_file, format!("{pid}\n{host}\n{port}\n")).map_err(|source| DaemonError::Io {
        path: pid_file.to_path_buf(),
        source,
    })
}

/// Remove the pid file.
pub fn clear(pid_file: &Path) {
    let _ = std::fs::remove_file(pid_file);
}

/// Whether a process is alive.
#[cfg(unix)]
pub fn is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    match kill(Pid::from_raw(pid), None) {
        Ok(()) => true,
        // The process exists but belongs to somebody else.
        Err(nix::errno::Errno::EPERM) => true,
        Err(_) => false,
    }
}

#[cfg(windows)]
pub fn is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }

    // SAFETY: `pid` is positive and OpenProcess only queries this process handle.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32) };
    if process.is_null() {
        // Match Unix behavior: a process we cannot inspect is still considered alive.
        return unsafe { GetLastError() } == ERROR_ACCESS_DENIED;
    }

    let mut exit_code = 0;
    // SAFETY: `process` is a valid handle and `exit_code` is writable.
    let queried = unsafe { GetExitCodeProcess(process, &mut exit_code) } != 0;
    // SAFETY: this function owns the handle returned by OpenProcess.
    unsafe { CloseHandle(process) };
    queried && exit_code == STILL_ACTIVE as u32
}

/// The address a health probe should use for a bound address.
///
/// A wildcard bind covers the loopback interface, so the probe can use it. A
/// specific address has to be probed where it actually listens.
pub fn probe_host(host: &str) -> &str {
    match host {
        "0.0.0.0" | "::" | "[::]" => "127.0.0.1",
        other => other,
    }
}

/// Whether the daemon answers `/health` on the given address.
pub fn health_ok(host: &str, port: u16) -> bool {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(2)))
        .timeout_global(Some(Duration::from_secs(3)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let url = format!("http://{}:{port}/health", probe_host(host));
    match agent.get(&url).call() {
        Ok(response) => (200..300).contains(&response.status().as_u16()),
        Err(_) => false,
    }
}

/// Work out whether the daemon is running.
///
/// A daemon counts as running only when the recorded process is alive **and**
/// the health endpoint answers. Either signal alone is unreliable: a pid can
/// be reused, and an unrelated process can hold the port.
pub fn state(pid_file: &Path, configured_host: &str, configured_port: u16) -> DaemonState {
    let Some(info) = read_info(pid_file) else {
        return DaemonState::Stopped;
    };
    if !is_alive(info.pid) {
        return DaemonState::Stale { pid: info.pid };
    }

    // Prefer what the daemon reported; fall back to the configured values.
    let host = info.host.unwrap_or_else(|| configured_host.to_string());
    let mut port = info.port.unwrap_or(configured_port);
    if !health_ok(&host, port)
        && port != configured_port
        && health_ok(configured_host, configured_port)
    {
        port = configured_port;
    }

    if health_ok(&host, port) {
        DaemonState::Running {
            pid: info.pid,
            host,
            port,
        }
    } else {
        DaemonState::Stale { pid: info.pid }
    }
}

/// Wait until the daemon answers `/health`, or the timeout expires.
pub fn wait_for_health(host: &str, port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if health_ok(host, port) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// What `daemonize` decided for us.
pub enum SpawnOutcome {
    /// We are the parent; the daemon is starting.
    Parent,
    /// We are the daemon.
    Child,
}

/// Start the server in the background with standard streams pointed at its log.
///
/// The caller waits for the health endpoint. Unix forks the current process;
/// Windows starts a foreground child process with its output redirected.
pub fn spawn(
    log_file: &Path,
    config_file: &Path,
    verbose: bool,
) -> Result<SpawnOutcome, DaemonError> {
    if let Some(parent) = log_file.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent).map_err(|source| DaemonError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .map_err(|source| DaemonError::Io {
            path: log_file.to_path_buf(),
            source,
        })?;

    let stdout = log.try_clone().map_err(|source| DaemonError::Io {
        path: log_file.to_path_buf(),
        source,
    })?;

    #[cfg(unix)]
    {
        let _ = (config_file, verbose);
        spawn_unix(stdout, log)
    }
    #[cfg(windows)]
    {
        spawn_windows(log_file, config_file, verbose, stdout, log)
    }
}

#[cfg(unix)]
fn spawn_unix(stdout: File, stderr: File) -> Result<SpawnOutcome, DaemonError> {
    let directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));

    let daemon = daemonize::Daemonize::new()
        .working_directory(directory)
        .stdout(stdout)
        .stderr(stderr)
        // 0o022 keeps the log readable, matching the documented 0644 mode.
        .umask(0o022u32);

    match daemon.execute() {
        daemonize::Outcome::Parent(Ok(_)) => Ok(SpawnOutcome::Parent),
        daemonize::Outcome::Parent(Err(err)) => Err(DaemonError::Daemonize(err.to_string())),
        daemonize::Outcome::Child(Ok(_)) => Ok(SpawnOutcome::Child),
        daemonize::Outcome::Child(Err(err)) => Err(DaemonError::Daemonize(err.to_string())),
    }
}

#[cfg(windows)]
fn spawn_windows(
    log_file: &Path,
    config_file: &Path,
    verbose: bool,
    stdout: File,
    stderr: File,
) -> Result<SpawnOutcome, DaemonError> {
    let executable = std::env::current_exe().map_err(|source| DaemonError::Io {
        path: PathBuf::from("current executable"),
        source,
    })?;
    let mut command = Command::new(&executable);
    command.arg("--config").arg(config_file);
    if verbose {
        command.arg("--verbose");
    }
    command
        .arg("run")
        .arg("--foreground")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .creation_flags(CREATE_NO_WINDOW);

    let child = command.spawn().map_err(|source| DaemonError::Io {
        path: log_file.to_path_buf(),
        source,
    })?;
    drop(child);
    Ok(SpawnOutcome::Parent)
}

/// Stop a daemon.
///
/// Unix sends `SIGTERM`, waits for a graceful exit, and escalates to `SIGKILL`.
/// Windows terminates the child process and waits for it to exit.
#[cfg(unix)]
pub fn stop(pid: i32, timeout: Duration) -> Result<bool, DaemonError> {
    if !is_alive(pid) {
        return Ok(false);
    }

    send_signal(pid, Signal::SIGTERM)?;

    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !is_alive(pid) {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    if is_alive(pid) {
        send_signal(pid, Signal::SIGKILL)?;
        // Give the kernel a moment to reap it.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && is_alive(pid) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    Ok(true)
}

#[cfg(windows)]
pub fn stop(pid: i32, timeout: Duration) -> Result<bool, DaemonError> {
    if !is_alive(pid) {
        return Ok(false);
    }

    // SAFETY: `pid` is positive and the requested access is limited to process termination.
    let process = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid as u32) };
    if process.is_null() {
        if !is_alive(pid) {
            return Ok(false);
        }
        return Err(DaemonError::Signal(format!(
            "could not open process {pid}: {}",
            std::io::Error::last_os_error()
        )));
    }

    // Windows does not provide a portable signal equivalent for this detached
    // child, so stop it explicitly. Capture the OS error before closing handle.
    // SAFETY: `process` is a valid handle opened with PROCESS_TERMINATE.
    let terminated = unsafe { TerminateProcess(process, 0) } != 0;
    let error = terminated.then_some(()).map_or_else(
        || Some(std::io::Error::last_os_error().to_string()),
        |_| None,
    );
    // SAFETY: this function owns the handle returned by OpenProcess.
    unsafe { CloseHandle(process) };
    if let Some(error) = error {
        return Err(DaemonError::Signal(format!(
            "could not terminate process {pid}: {error}"
        )));
    }

    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !is_alive(pid) {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    Err(DaemonError::Signal(format!(
        "timed out waiting for process {pid} to stop"
    )))
}

/// Send a Unix signal to a process.
#[cfg(unix)]
pub fn send_signal(pid: i32, signal: Signal) -> Result<(), DaemonError> {
    kill(Pid::from_raw(pid), signal).map_err(|err| DaemonError::Signal(err.to_string()))
}

/// Read the last few lines of the log file, for error reporting.
pub fn log_tail(log_file: &Path, lines: usize) -> String {
    let Ok(mut file) = std::fs::File::open(log_file) else {
        return String::new();
    };
    let mut text = String::new();
    if file.read_to_string(&mut text).is_err() {
        return String::new();
    }
    let collected: Vec<&str> = text.lines().rev().take(lines).collect();
    collected.into_iter().rev().collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pid_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("eggpass.pid");
        write_info(&path, 4321, "127.0.0.1", 8484).unwrap();
        let info = read_info(&path).unwrap();
        assert_eq!(info.pid, 4321);
        assert_eq!(info.port, Some(8484));
        assert_eq!(info.host.as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn a_pid_only_file_is_understood() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("eggpass.pid");
        std::fs::write(&path, "4321\n").unwrap();
        let info = read_info(&path).unwrap();
        assert_eq!(info.pid, 4321);
        assert_eq!(info.port, None);
    }

    #[test]
    fn a_missing_pid_file_means_stopped() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            state(&dir.path().join("eggpass.pid"), "127.0.0.1", 8484),
            DaemonState::Stopped
        );
    }

    #[test]
    fn a_dead_pid_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("eggpass.pid");
        // A pid that is almost certainly not running.
        write_info(&path, 999_999_999, "127.0.0.1", 8484).unwrap();
        assert_eq!(
            state(&path, "127.0.0.1", 8484),
            DaemonState::Stale { pid: 999_999_999 }
        );
    }

    #[test]
    fn this_process_is_alive() {
        assert!(is_alive(std::process::id() as i32));
        assert!(!is_alive(0));
        assert!(!is_alive(-1));
    }
}
