//! Daemon lifecycle: starting, finding and stopping the background server.
//!
//! The daemon is self-contained: a pid file, a health probe and a signal. No
//! launchd, no systemd, no root, and the same behaviour on macOS and Linux.
//!
//! The pid file holds two lines — the process id, and the port the daemon is
//! actually serving. The second line is what lets `grokproxy status` and
//! `grokproxy port` report the port that is live *right now*, which may differ
//! from the configured port after a `grokproxy port` change.

use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// How long `stop` waits for a graceful exit before sending `SIGKILL`.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonInfo {
    /// The process id.
    pub pid: i32,
    /// The port the daemon is serving, when it is recorded.
    pub port: Option<u16>,
}

/// Read the pid file.
pub fn read_info(pid_file: &Path) -> Option<DaemonInfo> {
    let text = std::fs::read_to_string(pid_file).ok()?;
    let mut lines = text.lines();
    let pid: i32 = lines.next()?.trim().parse().ok()?;
    let port = lines
        .next()
        .and_then(|line| line.trim().parse::<u16>().ok());
    Some(DaemonInfo { pid, port })
}

/// Write the pid file.
pub fn write_info(pid_file: &Path, pid: i32, port: u16) -> Result<(), DaemonError> {
    if let Some(parent) = pid_file.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|source| DaemonError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    std::fs::write(pid_file, format!("{pid}\n{port}\n")).map_err(|source| DaemonError::Io {
        path: pid_file.to_path_buf(),
        source,
    })
}

/// Remove the pid file.
pub fn clear(pid_file: &Path) {
    let _ = std::fs::remove_file(pid_file);
}

/// Whether a process is alive.
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

/// Whether the daemon answers `/health` on the given port.
pub fn health_ok(port: u16) -> bool {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(2)))
        .timeout_global(Some(Duration::from_secs(3)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    match agent.get(&format!("http://127.0.0.1:{port}/health")).call() {
        Ok(response) => (200..300).contains(&response.status().as_u16()),
        Err(_) => false,
    }
}

/// Work out whether the daemon is running.
///
/// A daemon counts as running only when the recorded process is alive **and**
/// the health endpoint answers. Either signal alone is unreliable: a pid can
/// be reused, and an unrelated process can hold the port.
pub fn state(pid_file: &Path, configured_port: u16) -> DaemonState {
    let Some(info) = read_info(pid_file) else {
        return DaemonState::Stopped;
    };
    if !is_alive(info.pid) {
        return DaemonState::Stale { pid: info.pid };
    }

    // Prefer the port the daemon reported; fall back to the configured one.
    let mut port = info.port.unwrap_or(configured_port);
    if !health_ok(port) && port != configured_port && health_ok(configured_port) {
        port = configured_port;
    }

    if health_ok(port) {
        DaemonState::Running {
            pid: info.pid,
            port,
        }
    } else {
        DaemonState::Stale { pid: info.pid }
    }
}

/// Wait until the daemon answers `/health`, or the timeout expires.
pub fn wait_for_health(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if health_ok(port) {
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

/// Fork into the background with the standard streams pointed at the log file.
///
/// The caller decides what to do next: the child starts the server, the parent
/// waits for the health endpoint.
pub fn spawn(log_file: &Path) -> Result<SpawnOutcome, DaemonError> {
    if let Some(parent) = log_file.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|source| DaemonError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
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

    let directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));

    let daemon = daemonize::Daemonize::new()
        .working_directory(directory)
        .stdout(stdout)
        .stderr(log)
        // 0o022 keeps the log readable, matching the documented 0644 mode.
        .umask(0o022u32);

    match daemon.execute() {
        daemonize::Outcome::Parent(Ok(_)) => Ok(SpawnOutcome::Parent),
        daemonize::Outcome::Parent(Err(err)) => Err(DaemonError::Daemonize(err.to_string())),
        daemonize::Outcome::Child(Ok(_)) => Ok(SpawnOutcome::Child),
        daemonize::Outcome::Child(Err(err)) => Err(DaemonError::Daemonize(err.to_string())),
    }
}

/// Stop a daemon.
///
/// Sends `SIGTERM`, waits for a graceful exit, and escalates to `SIGKILL` if
/// the daemon is still alive. Returns `true` when the daemon was running.
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

/// Send a signal to a process.
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
        let path = dir.path().join("grokproxy.pid");
        write_info(&path, 4321, 8484).unwrap();
        let info = read_info(&path).unwrap();
        assert_eq!(info.pid, 4321);
        assert_eq!(info.port, Some(8484));
    }

    #[test]
    fn a_pid_only_file_is_understood() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grokproxy.pid");
        std::fs::write(&path, "4321\n").unwrap();
        let info = read_info(&path).unwrap();
        assert_eq!(info.pid, 4321);
        assert_eq!(info.port, None);
    }

    #[test]
    fn a_missing_pid_file_means_stopped() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            state(&dir.path().join("grokproxy.pid"), 8484),
            DaemonState::Stopped
        );
    }

    #[test]
    fn a_dead_pid_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grokproxy.pid");
        // A pid that is almost certainly not running.
        write_info(&path, 999_999_999, 8484).unwrap();
        assert_eq!(state(&path, 8484), DaemonState::Stale { pid: 999_999_999 });
    }

    #[test]
    fn this_process_is_alive() {
        assert!(is_alive(std::process::id() as i32));
        assert!(!is_alive(0));
        assert!(!is_alive(-1));
    }
}
