//! Optional ChatGPT rate-limit reads from the local Codex app-server.
//!
//! This is a usage-only source. It invokes `codex app-server` over its
//! documented stdio JSON-RPC protocol, reads the account mode without
//! refreshing it, and asks only for rate limits. It never reads Codex
//! credential files or participates in Eggpass authentication or inference.

#[cfg(test)]
use std::ffi::OsStr;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStderr, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{QuotaWindow, UsageReport, WindowLabel};
use crate::provider::ProviderId;

/// The fixed label shown when a quota came from Codex rather than Eggpass OAuth.
pub const SOURCE_LABEL: &str = "Codex app-server (may differ from Eggpass inference account)";

/// One deadline covers process startup and the complete RPC exchange.
const SESSION_TIMEOUT: Duration = Duration::from_secs(8);
/// Maximum bytes accepted on one JSONL record.
const MAX_LINE_BYTES: usize = 256 * 1024;
/// Maximum total stdout bytes accepted during one app-server session.
const MAX_STDOUT_BYTES: usize = 1024 * 1024;
/// A small bounded queue prevents a noisy child from buffering unlimited lines.
const QUEUED_LINES: usize = 16;

/// Query the installed Codex CLI, returning `None` for every optional-source failure.
pub fn collect() -> Option<UsageReport> {
    let mut command = Command::new("codex");
    command.arg("app-server");
    collect_with_command(command, SESSION_TIMEOUT)
}

/// Run one app-server session. Kept injectable so tests can use a fake process.
fn collect_with_command(mut command: Command, timeout: Duration) -> Option<UsageReport> {
    let deadline = Instant::now().checked_add(timeout)?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = command.spawn().ok()?;
    let mut process = ManagedChild::new(child);
    let stdin = process.child.stdin.take()?;
    process.stdin = Some(stdin);
    let stdout = process.child.stdout.take()?;
    let stderr = process.child.stderr.take()?;

    // Stderr is drained on a separate thread and discarded. No child output is
    // retained, logged, or allowed to block the JSON-RPC channel.
    drain_stderr(stderr).ok()?;
    let responses = spawn_stdout_reader(stdout)?;

    let initialize = json!({
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": {
                "name": "eggpass",
                "title": "Eggpass",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {}
        }
    });
    if !send_message(process.stdin.as_mut()?, &initialize)
        || receive_response(&responses, 1, deadline).is_none()
    {
        return None;
    }

    if !send_message(process.stdin.as_mut()?, &json!({ "method": "initialized" })) {
        return None;
    }

    let account_request = json!({
        "id": 2,
        "method": "account/read",
        "params": { "refreshToken": false }
    });
    if !send_message(process.stdin.as_mut()?, &account_request) {
        return None;
    }
    let account = receive_response(&responses, 2, deadline)?;
    if account
        .get("account")
        .and_then(|account| account.get("type"))
        .and_then(Value::as_str)
        != Some("chatgpt")
    {
        return None;
    }

    let rate_limits_request = json!({
        "id": 3,
        "method": "account/rateLimits/read"
    });
    if !send_message(process.stdin.as_mut()?, &rate_limits_request) {
        return None;
    }
    let rate_limits = receive_response(&responses, 3, deadline)?;
    map_rate_limits(&rate_limits)
}

/// Ensure the direct child is closed and reaped on every return path.
struct ManagedChild {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl ManagedChild {
    fn new(child: Child) -> Self {
        Self { child, stdin: None }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        // Closing stdin lets a well-behaved server observe EOF before it is
        // terminated. kill + wait also handles a server that ignores EOF.
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Drain the child's stderr without retaining its contents.
fn drain_stderr(stderr: ChildStderr) -> io::Result<()> {
    thread::Builder::new()
        .name("eggpass-codex-stderr".to_string())
        .spawn(move || {
            let mut stderr = stderr;
            let mut buffer = [0_u8; 8192];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        })
        .map(|_| ())
}

/// Start a bounded stdout reader; protocol bytes never accumulate unboundedly.
fn spawn_stdout_reader(stdout: std::process::ChildStdout) -> Option<Receiver<Result<Vec<u8>, ()>>> {
    let (sender, receiver) = mpsc::sync_channel(QUEUED_LINES);
    thread::Builder::new()
        .name("eggpass-codex-stdout".to_string())
        .spawn(move || read_stdout_lines(stdout, sender))
        .ok()?;
    Some(receiver)
}

fn read_stdout_lines(stdout: std::process::ChildStdout, sender: SyncSender<Result<Vec<u8>, ()>>) {
    let mut reader = BufReader::new(stdout);
    let mut total_bytes = 0_usize;
    loop {
        match read_bounded_line(&mut reader, MAX_LINE_BYTES) {
            Ok(Some(line)) => {
                total_bytes = match total_bytes.checked_add(line.len().saturating_add(1)) {
                    Some(total) if total <= MAX_STDOUT_BYTES => total,
                    _ => {
                        let _ = sender.send(Err(()));
                        break;
                    }
                };
                if sender.send(Ok(line)).is_err() {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => {
                let _ = sender.try_send(Err(()));
                break;
            }
        }
    }
}

/// Read one newline-terminated record without allowing it to exceed `limit`.
fn read_bounded_line(reader: &mut impl BufRead, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unterminated JSONL record",
            ));
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let content_len = newline.map_or(consumed, |index| index);
        if line
            .len()
            .checked_add(content_len)
            .is_none_or(|length| length > limit)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "JSONL record exceeds the limit",
            ));
        }
        line.extend_from_slice(&available[..content_len]);
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}

fn send_message(stdin: &mut ChildStdin, message: &Value) -> bool {
    serde_json::to_vec(message).ok().is_some_and(|message| {
        stdin.write_all(&message).is_ok() && stdin.write_all(b"\n").is_ok() && stdin.flush().is_ok()
    })
}

/// Read a response matching `id`; notifications and other response IDs are ignored.
fn receive_response(
    responses: &Receiver<Result<Vec<u8>, ()>>,
    id: u64,
    deadline: Instant,
) -> Option<Value> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let line = responses.recv_timeout(remaining).ok()?.ok()?;
        let value: Value = serde_json::from_slice(&line).ok()?;
        let object = value.as_object()?;

        let Some(response_id) = object.get("id") else {
            // A JSON-RPC notification has a method and no id. Other id-less
            // objects are malformed and cannot be a useful response.
            if object.get("method").and_then(Value::as_str).is_some() {
                continue;
            }
            return None;
        };
        if response_id != &Value::from(id) {
            continue;
        }
        if object.contains_key("error") {
            return None;
        }
        return object.get("result").cloned();
    }
}

/// Convert the documented rate-limit result without inventing missing values.
fn map_rate_limits(result: &Value) -> Option<UsageReport> {
    let mut windows = Vec::new();
    let buckets = result
        .get("rateLimitsByLimitId")
        .and_then(Value::as_object)
        .filter(|buckets| !buckets.is_empty());

    if let Some(buckets) = buckets {
        for (bucket_id, snapshot) in buckets {
            append_snapshot_windows(&mut windows, snapshot, Some(bucket_id));
        }
    } else if let Some(snapshot) = result.get("rateLimits") {
        append_snapshot_windows(&mut windows, snapshot, None);
    }

    if windows.is_empty() {
        return None;
    }

    Some(UsageReport {
        provider: ProviderId::OpenAi,
        plan: None,
        windows,
        breakdown: Vec::new(),
        credits: None,
        state: None,
    })
}

fn append_snapshot_windows(
    windows: &mut Vec<QuotaWindow>,
    snapshot: &Value,
    bucket_id: Option<&str>,
) {
    for (field, role) in [("primary", "Primary"), ("secondary", "Secondary")] {
        let Some(window) = snapshot.get(field).filter(|window| window.is_object()) else {
            continue;
        };
        let used_percent = window
            .get("usedPercent")
            .and_then(Value::as_f64)
            .filter(|percent| percent.is_finite() && (0.0..=100.0).contains(percent));
        let resets_at = window.get("resetsAt").and_then(Value::as_i64);
        let duration_secs = window
            .get("windowDurationMins")
            .and_then(Value::as_i64)
            .filter(|minutes| *minutes >= 0)
            .and_then(|minutes| minutes.checked_mul(60));

        // A window with no usable quota fields is not a zero-usage report.
        if used_percent.is_none() && resets_at.is_none() && duration_secs.is_none() {
            continue;
        }

        let label = match bucket_id {
            Some(bucket_id) => format!("{} {role}", safe_bucket_id(bucket_id)),
            None => role.to_string(),
        };
        windows.push(QuotaWindow {
            label: WindowLabel::Other(label),
            used_percent,
            resets_at,
            duration_secs,
        });
    }
}

/// Escape control and non-ASCII bucket-id bytes before they reach terminal output.
fn safe_bucket_id(bucket_id: &str) -> String {
    let mut safe = String::new();
    for byte in bucket_id.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
            safe.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(safe, "%{byte:02X}");
        }
        if safe.len() >= 64 {
            safe.truncate(64);
            safe.push_str("...");
            break;
        }
    }
    if safe.is_empty() {
        "unknown".to_string()
    } else {
        safe
    }
}

#[cfg(test)]
fn executable_command(executable: &OsStr) -> Command {
    let mut command = Command::new(executable);
    command.arg("app-server");
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    static FAKE_SERVER: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();

    fn fake_server() -> &'static PathBuf {
        let (_, executable) = FAKE_SERVER.get_or_init(|| {
            let directory = tempfile::tempdir().expect("temporary fake-server directory");
            let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/codex_app_server.rs");
            let executable = directory.path().join(if cfg!(windows) {
                "fake-codex.exe"
            } else {
                "fake-codex"
            });
            let output = Command::new("rustc")
                .arg("--edition=2024")
                .arg(source)
                .arg("-o")
                .arg(&executable)
                .output()
                .expect("rustc is available while running Cargo tests");
            assert!(
                output.status.success(),
                "fake app-server failed to compile: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            (directory, executable)
        });
        executable
    }

    fn fake_command(mode: &str) -> Command {
        let mut command = executable_command(fake_server().as_os_str());
        command.env("EGGPASS_CODEX_TEST_MODE", mode);
        command
    }

    fn collect_fake_with_timeout(mode: &str, timeout: Duration) -> Option<UsageReport> {
        collect_with_command(fake_command(mode), timeout)
    }

    fn collect_fake(mode: &str) -> Option<UsageReport> {
        collect_fake_with_timeout(mode, Duration::from_secs(2))
    }

    #[test]
    fn account_rpc_flow_uses_only_read_methods_and_chatgpt_mode() {
        let report = collect_fake("success").expect("the fake account has quota data");
        assert_eq!(report.provider, ProviderId::OpenAi);
        assert_eq!(report.windows.len(), 2);
        assert_eq!(report.windows[0].used_percent, Some(42.0));
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("private-codex-account@example.invalid"));

        let stderr_report = collect_fake("stderr").expect("stderr does not break protocol reads");
        let stderr_report = serde_json::to_string(&stderr_report).unwrap();
        assert!(!stderr_report.contains("do-not-print-this-private-diagnostic"));
    }

    #[test]
    fn a_burst_of_unrelated_notifications_does_not_hide_the_matching_response() {
        let report = collect_fake("notification_burst")
            .expect("the bounded reader must backpressure rather than drop notifications");
        assert_eq!(report.windows.len(), 2);
    }

    #[test]
    fn account_modes_without_chatgpt_usage_are_unavailable() {
        for mode in ["no_account", "api_key", "other_account"] {
            assert!(collect_fake(mode).is_none(), "mode {mode} must not be used");
        }
    }

    #[test]
    fn missing_executable_and_spawn_failure_are_optional() {
        let missing = tempfile::tempdir().unwrap().path().join("missing-codex");
        assert!(
            collect_with_command(
                executable_command(missing.as_os_str()),
                Duration::from_millis(100)
            )
            .is_none()
        );
    }

    #[test]
    fn early_exit_rpc_errors_and_malformed_output_are_optional() {
        for mode in [
            "early_exit",
            "initialize_error",
            "account_error",
            "rate_limits_error",
            "malformed",
            "oversized",
            "stdout_total",
        ] {
            assert!(collect_fake(mode).is_none(), "mode {mode} must fall back");
        }
    }

    #[test]
    fn a_stalled_or_mismatched_rpc_cannot_hold_usage_open() {
        for mode in [
            "stall_initialize",
            "stall_account",
            "stall_rate_limits",
            "wrong_id",
            "missing_id",
        ] {
            let started = Instant::now();
            assert!(
                collect_fake_with_timeout(mode, Duration::from_millis(200)).is_none(),
                "mode {mode} must fall back"
            );
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "mode {mode} exceeded the deadline"
            );
        }
    }

    #[test]
    fn a_populated_bucket_map_is_preferred_and_keeps_all_windows() {
        let report = collect_fake("multiple_buckets").expect("the map has quota data");
        assert_eq!(report.windows.len(), 3);
        let labels: Vec<_> = report
            .windows
            .iter()
            .map(|window| window.label.display())
            .collect();
        assert_eq!(
            labels,
            ["codex Primary", "codex Secondary", "other-model Primary"]
        );
        assert_eq!(report.windows[0].used_percent, Some(42.0));
        assert_eq!(report.windows[0].resets_at, Some(1_800_000_000));
        assert_eq!(report.windows[0].duration_secs, Some(18_000));
        assert!(
            !report
                .windows
                .iter()
                .any(|window| window.used_percent == Some(99.0))
        );
    }

    #[test]
    fn legacy_and_empty_bucket_map_results_are_supported() {
        for mode in ["legacy", "empty_buckets"] {
            let report = collect_fake(mode).expect("legacy quota data is available");
            assert_eq!(report.windows.len(), 1);
            assert_eq!(report.windows[0].label.display(), "Primary");
            assert_eq!(report.windows[0].used_percent, Some(9.0));
        }
    }

    #[test]
    fn null_and_missing_fields_stay_unavailable_instead_of_becoming_zero() {
        let report = collect_fake("nullable_fields").expect("a real zero percentage is usable");
        assert_eq!(report.windows.len(), 1);
        assert_eq!(report.windows[0].used_percent, Some(0.0));
        assert_eq!(report.windows[0].duration_secs, Some(300));
        assert_eq!(report.windows[0].resets_at, None);

        assert!(collect_fake("no_data").is_none());
    }

    #[test]
    fn unknown_plan_and_limit_states_do_not_masquerade_as_generic_states() {
        let report = collect_fake("unknown_variants").expect("the quota window is usable");
        assert_eq!(report.plan, None);
        assert_eq!(report.state, None);
    }

    #[test]
    fn invalid_values_are_ignored_and_duration_conversion_is_checked() {
        let invalid_percent = json!({
            "rateLimits": { "primary": { "usedPercent": 150, "resetsAt": 1_800_000_000 } }
        });
        let report = map_rate_limits(&invalid_percent).expect("the reset is still useful");
        assert_eq!(report.windows[0].used_percent, None);
        assert_eq!(report.windows[0].resets_at, Some(1_800_000_000));

        let overflow = json!({
            "rateLimits": { "primary": { "usedPercent": null, "windowDurationMins": i64::MAX } }
        });
        assert!(map_rate_limits(&overflow).is_none());
    }

    #[test]
    fn bucket_ids_cannot_inject_terminal_control_sequences() {
        let result = json!({
            "rateLimitsByLimitId": {
                "codex\u{1b}[31m": { "primary": { "usedPercent": 10 } }
            }
        });
        let report = map_rate_limits(&result).unwrap();
        assert_eq!(report.windows[0].label.display(), "codex%1B%5B31m Primary");
    }

    #[test]
    fn each_jsonl_record_is_bounded_and_must_be_terminated() {
        assert!(read_bounded_line(&mut BufReader::new(&b"12345\n"[..]), 4).is_err());
        assert!(read_bounded_line(&mut BufReader::new(&b"unterminated"[..]), 20).is_err());
        assert_eq!(
            read_bounded_line(&mut BufReader::new(&b"{}\n"[..]), 2).unwrap(),
            Some(b"{}".to_vec())
        );
    }

    #[test]
    fn notification_records_are_ignored_until_the_matching_id_arrives() {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(br#"{"method":"account/updated"}"#.to_vec()))
            .unwrap();
        sender
            .send(Ok(br#"{"id":99,"result":{}}"#.to_vec()))
            .unwrap();
        sender
            .send(Ok(br#"{"id":1,"result":{"ready":true}}"#.to_vec()))
            .unwrap();
        let result = receive_response(&receiver, 1, Instant::now() + Duration::from_secs(1));
        assert_eq!(result, Some(json!({"ready": true})));
    }

    #[test]
    fn source_label_does_not_expose_the_account_identity() {
        assert!(SOURCE_LABEL.contains("Codex app-server"));
        assert!(SOURCE_LABEL.contains("may differ from Eggpass inference account"));
        assert!(!SOURCE_LABEL.contains('@'));
    }
}
