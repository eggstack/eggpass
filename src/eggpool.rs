//! EggPool ticket, enrollment, and reverse-client support.
//!
//! The one-time enrollment token is accepted only on the TLS-protected reverse
//! connection and the private enrollment callback. EggPool provisions the
//! durable mTLS pair through that same reverse tunnel; the callback never
//! accepts arbitrary forwarding targets.

use std::io::Write;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use eggress_protocol_reverse::client::{ReverseClient, ReverseClientConfig};
use eggress_protocol_reverse::metrics::ReverseMetrics;
use eggress_protocol_reverse::tls::ReverseClientTlsConfig;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;
use zeroize::{Zeroize, Zeroizing};

use crate::config::{Config, EggpoolSettings};
use crate::keys::KeyStore;
use crate::paths::{Paths, restrict_permissions};

/// The reverse-proxied endpoint used only to receive the EggPool client cert.
pub const ENROLLMENT_PATH: &str = "/v1/eggpool/enroll";
/// The one-time ticket lifetime in seconds.
pub const TICKET_TTL_SECONDS: i64 = 600;
const MAX_TICKET_BYTES: usize = 2048;
const MAX_CERT_PEM_BYTES: usize = 64 * 1024;
const MAX_KEY_PEM_BYTES: usize = 64 * 1024;
const STATUS_STALE_SECONDS: i64 = 10;

/// Sensitive local state. The API key itself is kept only in the emitted
/// ticket and the normal key store retains only its salted hash.
#[derive(Serialize, Deserialize)]
struct EggpoolSecrets {
    version: u8,
    agent: String,
    api_key_id: u32,
    ca_sha256: String,
    #[serde(default)]
    enroll_token: Option<String>,
    #[serde(default)]
    expires_at: Option<i64>,
    #[serde(default)]
    client_cert_pem: Option<String>,
    #[serde(default)]
    client_key_pem: Option<String>,
}

impl std::fmt::Debug for EggpoolSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EggpoolSecrets")
            .field("version", &self.version)
            .field("agent", &self.agent)
            .field("api_key_id", &self.api_key_id)
            .field("ca_sha256", &self.ca_sha256)
            .field(
                "enroll_token",
                &self.enroll_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at", &self.expires_at)
            .field(
                "client_cert_pem",
                &self.client_cert_pem.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "client_key_pem",
                &self.client_key_pem.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl Drop for EggpoolSecrets {
    fn drop(&mut self) {
        if let Some(value) = &mut self.enroll_token {
            value.zeroize();
        }
        if let Some(value) = &mut self.client_key_pem {
            value.zeroize();
        }
        if let Some(value) = &mut self.client_cert_pem {
            value.zeroize();
        }
    }
}

#[derive(Serialize)]
struct Ticket<'a> {
    #[serde(rename = "v")]
    version: u8,
    control: &'a str,
    server_name: &'a str,
    ca_sha256: &'a str,
    enroll: String,
    expires_at: i64,
    agent: &'a str,
    eggpass: &'static str,
    api_key: String,
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        self.enroll.zeroize();
        self.api_key.zeroize();
    }
}

/// Inputs that bind one enrollment ticket to an EggPool control endpoint and
/// the currently configured loopback listener.
pub struct TicketSetup<'a> {
    pub control: &'a str,
    pub server_name: &'a str,
    pub ca_path: &'a Path,
    pub label: &'a str,
    pub live_host: &'a str,
    pub live_port: u16,
}

/// Generate a ticket, persist its pending enrollment secret, and update the
/// non-secret EggPool configuration. The caller restarts the daemon and prints
/// the returned line only after those operations succeed.
pub fn create_ticket(
    paths: &Paths,
    config: &Config,
    setup: TicketSetup<'_>,
) -> Result<Zeroizing<String>, String> {
    let TicketSetup {
        control,
        server_name,
        ca_path,
        label,
        live_host,
        live_port,
    } = setup;
    if !crate::config::is_loopback_host(live_host) {
        return Err("EggPool requires EggPass to be bound to a loopback address".to_string());
    }
    if !crate::config::is_loopback_host(&config.host) {
        return Err(
            "EggPool cannot be enabled while the configured host is not loopback".to_string(),
        );
    }
    if live_host != config.host || live_port != config.port {
        return Err("the live EggPass listener differs from the current configuration; restart EggPass and retry".to_string());
    }
    validate_control(control)?;
    validate_server_name(server_name)?;
    validate_label(label)?;

    let ca_pem = std::fs::read(ca_path)
        .map_err(|err| format!("could not read the EggPool CA file: {err}"))?;
    let ca_sha256 = ca_digest(&ca_pem)?;
    let old = load_secrets(&paths.eggpool_secret_file())?;

    paths
        .ensure_dir()
        .map_err(|err| format!("could not secure the EggPass data directory: {err}"))?;
    let key_store = KeyStore::open(paths.key_store_file())
        .map_err(|err| format!("could not open the EggPass key store: {err}"))?;
    let (api_key_id, api_key) = key_store
        .add(Some(format!("EggPool agent {label}")))
        .map_err(|err| format!("could not create an EggPool API key: {err}"))?;

    let mut random = [0u8; 32];
    rand::rng().fill_bytes(&mut random);
    let mut enroll_token = URL_SAFE_NO_PAD.encode(random);
    random.zeroize();
    let expires_at = crate::timeutil::now_unix() + TICKET_TTL_SECONDS;

    let mut state = EggpoolSecrets {
        version: 1,
        agent: label.to_string(),
        api_key_id,
        ca_sha256: ca_sha256.clone(),
        enroll_token: Some(enroll_token.clone()),
        expires_at: Some(expires_at),
        client_cert_pem: None,
        client_key_pem: None,
    };
    if let Err(err) = write_secrets(&paths.eggpool_secret_file(), &state) {
        let _ = key_store.revoke(api_key_id);
        enroll_token.zeroize();
        return Err(err);
    }

    let settings = EggpoolSettings {
        enabled: true,
        control: Some(control.to_string()),
        server_name: Some(server_name.to_string()),
        ca: Some(ca_path.to_path_buf()),
        label: Some(label.to_string()),
    };
    if let Err(err) = crate::config::set_eggpool_settings(paths.config_file(), &settings) {
        let _ = key_store.revoke(api_key_id);
        let _ = remove_file_if_present(&paths.eggpool_secret_file());
        enroll_token.zeroize();
        return Err(format!("could not update the EggPool config: {err}"));
    }

    if let Some(previous) = old
        && previous.api_key_id != api_key_id
        && key_store
            .entries()
            .iter()
            .any(|entry| entry.id == previous.api_key_id)
    {
        key_store
            .revoke(previous.api_key_id)
            .map_err(|err| format!("could not revoke the previous EggPool API key: {err}"))?;
    }

    let ticket = Ticket {
        version: 1,
        control,
        server_name,
        ca_sha256: &ca_sha256,
        enroll: std::mem::take(&mut enroll_token),
        expires_at,
        agent: label,
        eggpass: env!("CARGO_PKG_VERSION"),
        api_key,
    };
    let payload = Zeroizing::new(
        serde_json::to_vec(&ticket)
            .map_err(|err| format!("could not encode the EggPool ticket: {err}"))?,
    );
    let encoded = Zeroizing::new(URL_SAFE_NO_PAD.encode(payload.as_slice()));
    let line = Zeroizing::new(format!("epagent1.{}", encoded.as_str()));
    if line.len() > MAX_TICKET_BYTES || !line.is_ascii() || line.chars().any(char::is_whitespace) {
        let _ = key_store.revoke(api_key_id);
        let _ = remove_file_if_present(&paths.eggpool_secret_file());
        state.enroll_token = None;
        return Err("the EggPool ticket exceeded the 2 KiB single-line limit".to_string());
    }

    // The live daemon address is checked by the caller and used for the fixed
    // target; config remains the source of truth for the next daemon start.
    let _ = (live_host, live_port, ca_pem);
    enroll_token.zeroize();
    Ok(line)
}

/// Remove the EggPool API key, pending token, mTLS credentials, and config
/// enablement. The caller stops a running daemon before this operation and may restart it afterward if another API key remains.
pub fn revoke(paths: &Paths, config: &Config) -> Result<(), String> {
    let state = load_secrets(&paths.eggpool_secret_file())?;
    if let Some(state) = state {
        let key_store = KeyStore::open(paths.key_store_file())
            .map_err(|err| format!("could not open the EggPass key store: {err}"))?;
        if key_store
            .entries()
            .iter()
            .any(|entry| entry.id == state.api_key_id)
        {
            key_store
                .revoke(state.api_key_id)
                .map_err(|err| format!("could not revoke the EggPool API key: {err}"))?;
        }
    }

    let mut settings = config.eggpool.clone();
    settings.enabled = false;
    crate::config::set_eggpool_settings(paths.config_file(), &settings)
        .map_err(|err| format!("could not disable EggPool in the config: {err}"))?;
    remove_file_if_present(&paths.eggpool_secret_file())?;
    remove_file_if_present(&paths.eggpool_status_file())?;
    Ok(())
}

/// Status text intentionally contains no credentials or certificate data.
pub fn status_summary(settings: &EggpoolSettings, paths: &Paths, daemon_running: bool) -> String {
    if !settings.enabled {
        return "disabled".to_string();
    }
    if !daemon_running {
        return "configured; EggPass daemon is stopped".to_string();
    }

    let record = match std::fs::read(paths.eggpool_status_file()) {
        Ok(bytes) => serde_json::from_slice::<TunnelStatus>(&bytes).ok(),
        Err(_) => None,
    };
    let Some(record) = record else {
        return "starting or reconnecting".to_string();
    };
    let state =
        if crate::timeutil::now_unix().saturating_sub(record.updated_at) > STATUS_STALE_SECONDS {
            "disconnected"
        } else {
            record.state.as_str()
        };
    format!("{state} (agent {}, {})", record.agent, record.control)
}

/// State shared by the HTTP server and the reverse-client supervisor.
pub struct EnrollmentStore {
    path: PathBuf,
    ca_pem: Vec<u8>,
    server_name: String,
    changed: Arc<Notify>,
    lock: Mutex<()>,
}

impl EnrollmentStore {
    pub(crate) fn new(
        path: PathBuf,
        ca_pem: Vec<u8>,
        server_name: String,
        changed: Arc<Notify>,
    ) -> Self {
        Self {
            path,
            ca_pem,
            server_name,
            changed,
            lock: Mutex::new(()),
        }
    }

    /// Check whether a bearer token is the still-live one-time enrollment token.
    pub fn authorizes(&self, token: &str) -> bool {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Ok(Some(mut state)) = load_secrets(&self.path) else {
            return false;
        };
        let live = state
            .expires_at
            .is_some_and(|at| at >= crate::timeutil::now_unix());
        let accepted = state.enroll_token.as_deref().is_some_and(|expected| {
            crate::keys::constant_time_eq(expected.as_bytes(), token.as_bytes())
        });
        if let Some(value) = state.enroll_token.as_mut() {
            value.zeroize();
        }
        live && accepted
    }

    /// Validate and persist an mTLS certificate/key bundle, consuming the
    /// enrollment token only after all validation succeeds.
    pub fn complete(&self, token: &str, body: &[u8]) -> Result<(), String> {
        if body.len() > MAX_CERT_PEM_BYTES + MAX_KEY_PEM_BYTES + 4096 {
            return Err("EggPool enrollment body is too large".to_string());
        }
        let mut bundle: EnrollmentBundle = serde_json::from_slice(body)
            .map_err(|_| "invalid EggPool enrollment payload".to_string())?;
        validate_client_material(&bundle.client_cert_pem, &bundle.client_key_pem)?;

        let tls = ReverseClientTlsConfig {
            ca_pem: Some(self.ca_pem.clone()),
            server_name: self.server_name.clone(),
            client_cert_pem: Some(bundle.client_cert_pem.as_bytes().to_vec()),
            client_key_pem: Some(bundle.client_key_pem.as_bytes().to_vec()),
        };
        tls.validate()
            .map_err(|_| "EggPool supplied an invalid mTLS certificate/key pair".to_string())?;

        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = load_secrets(&self.path)?
            .ok_or_else(|| "EggPool enrollment is no longer pending".to_string())?;
        let live = state
            .expires_at
            .is_some_and(|at| at >= crate::timeutil::now_unix());
        let accepted = state.enroll_token.as_deref().is_some_and(|expected| {
            crate::keys::constant_time_eq(expected.as_bytes(), token.as_bytes())
        });
        if !live || !accepted {
            return Err("EggPool enrollment is expired or already consumed".to_string());
        }

        if let Some(value) = &mut state.enroll_token {
            value.zeroize();
        }
        state.enroll_token = None;
        state.expires_at = None;
        state.client_cert_pem = Some(std::mem::take(&mut bundle.client_cert_pem));
        state.client_key_pem = Some(std::mem::take(&mut bundle.client_key_pem));
        write_secrets(&self.path, &state)?;
        self.changed.notify_one();
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollmentBundle {
    client_cert_pem: String,
    client_key_pem: String,
}

impl Drop for EnrollmentBundle {
    fn drop(&mut self) {
        self.client_cert_pem.zeroize();
        self.client_key_pem.zeroize();
    }
}

/// Extract only a Bearer token from the exact enrollment route.
pub fn enrollment_bearer(header: Option<&str>) -> Option<&str> {
    let (scheme, value) = header?.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let value = value.trim();
    (!value.is_empty() && !value.chars().any(char::is_whitespace)).then_some(value)
}

/// Handle the internal enrollment callback. Request body and bearer material
/// are zeroized before the request is dropped.
pub fn handle_enrollment(
    mut request: crate::server::Request,
    store: &EnrollmentStore,
) -> crate::server::Response {
    let mut token = enrollment_bearer(request.header("authorization")).map(str::to_string);
    let result = token
        .as_deref()
        .ok_or_else(|| "missing enrollment authorization".to_string())
        .and_then(|token| store.complete(token, &request.body));
    scrub_enrollment_request(&mut request);
    if let Some(value) = &mut token {
        value.zeroize();
    }

    match result {
        Ok(()) => crate::server::Response::bytes(
            204,
            vec![("Cache-Control".to_string(), "no-store".to_string())],
            Vec::new(),
        ),
        Err(_) => crate::server::openai_error(
            400,
            "invalid or expired EggPool enrollment payload",
            "invalid_request_error",
        ),
    }
}

/// Zero sensitive parts of an enrollment request on every response path.
pub(crate) fn scrub_enrollment_request(request: &mut crate::server::Request) {
    request.body.zeroize();
    for (name, value) in &mut request.headers {
        if name.eq_ignore_ascii_case("authorization") {
            value.zeroize();
        }
    }
}

/// A running Eggress client and its local mTLS enrollment endpoint state.
pub struct TunnelRuntime {
    stop: Arc<AtomicBool>,
    changed: Arc<Notify>,
    join: Option<JoinHandle<()>>,
    enrollment: Arc<EnrollmentStore>,
}

impl TunnelRuntime {
    pub fn enrollment_store(&self) -> Arc<EnrollmentStore> {
        Arc::clone(&self.enrollment)
    }

    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.changed.notify_one();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for TunnelRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Start a dedicated Tokio runtime for the Eggress reverse client. The client
/// always relays to the fixed loopback EggPass listener, never a ticket target.
pub fn start_runtime(
    settings: &EggpoolSettings,
    secret_path: PathBuf,
    status_path: PathBuf,
    local_host: String,
    local_port: u16,
) -> Result<TunnelRuntime, String> {
    let control = settings
        .control
        .as_ref()
        .ok_or_else(|| "EggPool control endpoint is missing".to_string())?
        .clone();
    validate_control(&control)?;
    let server_name = settings
        .server_name
        .as_ref()
        .ok_or_else(|| "EggPool TLS server name is missing".to_string())?
        .clone();
    let ca_path = settings
        .ca
        .as_ref()
        .ok_or_else(|| "EggPool CA path is missing".to_string())?;
    let ca_pem = std::fs::read(ca_path)
        .map_err(|err| format!("could not read the EggPool CA file: {err}"))?;
    validate_server_name(&server_name)?;
    ReverseClientTlsConfig {
        ca_pem: Some(ca_pem.clone()),
        server_name: server_name.clone(),
        client_cert_pem: None,
        client_key_pem: None,
    }
    .validate()
    .map_err(|_| "EggPool CA or TLS server name is invalid".to_string())?;

    let changed = Arc::new(Notify::new());
    let enrollment = Arc::new(EnrollmentStore::new(
        secret_path.clone(),
        ca_pem,
        server_name.clone(),
        Arc::clone(&changed),
    ));
    write_status(status_path.as_path(), settings, "starting");
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread_changed = Arc::clone(&changed);
    let thread_settings = settings.clone();
    let thread_status = status_path;
    let join = thread::Builder::new()
        .name("eggpass-eggpool-reverse".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(_) => {
                    write_status(&thread_status, &thread_settings, "error");
                    return;
                }
            };
            runtime.block_on(supervise_client(
                thread_settings,
                secret_path,
                thread_status,
                local_host,
                local_port,
                thread_changed,
                thread_stop,
            ));
        })
        .map_err(|err| format!("could not start EggPool reverse client: {err}"))?;

    Ok(TunnelRuntime {
        stop,
        changed,
        join: Some(join),
        enrollment,
    })
}

async fn supervise_client(
    settings: EggpoolSettings,
    secret_path: PathBuf,
    status_path: PathBuf,
    local_host: String,
    local_port: u16,
    changed: Arc<Notify>,
    stop: Arc<AtomicBool>,
) {
    loop {
        if stop.load(Ordering::SeqCst) {
            write_status(&status_path, &settings, "stopped");
            return;
        }
        let state = match load_secrets(&secret_path) {
            Ok(Some(state)) => state,
            Ok(None) => {
                write_status(&status_path, &settings, "enrollment_required");
                changed.notified().await;
                continue;
            }
            Err(_) => {
                write_status(&status_path, &settings, "error");
                tokio::select! {
                    _ = changed.notified() => {},
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {},
                }
                continue;
            }
        };

        let has_pair = state.client_cert_pem.is_some() && state.client_key_pem.is_some();
        let pending = state.enroll_token.is_some()
            && state
                .expires_at
                .is_some_and(|at| at >= crate::timeutil::now_unix());
        if !has_pair && !pending {
            write_status(&status_path, &settings, "enrollment_expired");
            changed.notified().await;
            continue;
        }

        let config = match client_config(&settings, &state, &local_host, local_port) {
            Ok(config) => config,
            Err(_) => {
                write_status(&status_path, &settings, "error");
                tokio::select! {
                    _ = changed.notified() => {},
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {},
                }
                continue;
            }
        };
        let metrics = Arc::new(ReverseMetrics::new());
        let mut client = ReverseClient::new(config);
        client.set_metrics(Arc::clone(&metrics));
        let client = Arc::new(client);
        let mut running = Box::pin(client.run());
        let mut tick = tokio::time::interval(Duration::from_secs(1));

        loop {
            tokio::select! {
                result = &mut running => {
                    let state = if result.is_err() { "error" } else { "disconnected" };
                    write_status(&status_path, &settings, state);
                    break;
                }
                _ = changed.notified() => {
                    client.shutdown();
                    break;
                }
                _ = tick.tick() => {
                    if pending
                        && state
                            .expires_at
                            .is_some_and(|at| at < crate::timeutil::now_unix())
                    {
                        client.shutdown();
                        write_status(&status_path, &settings, "enrollment_expired");
                        break;
                    }
                    let active = metrics.control_connections_active.load(Ordering::Relaxed);
                    let state = if active > 0 { "connected" } else { "reconnecting" };
                    write_status(&status_path, &settings, state);
                }
            }
            if stop.load(Ordering::SeqCst) {
                client.shutdown();
                write_status(&status_path, &settings, "stopped");
                return;
            }
        }
        drop(running);
        drop(state);
        if stop.load(Ordering::SeqCst) {
            write_status(&status_path, &settings, "stopped");
            return;
        }
        // A credential update or a fatal client error should not spin.
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn client_config(
    settings: &EggpoolSettings,
    state: &EggpoolSecrets,
    local_host: &str,
    local_port: u16,
) -> Result<ReverseClientConfig, String> {
    let control = settings
        .control
        .as_deref()
        .ok_or_else(|| "missing EggPool control endpoint".to_string())?;
    let address = control
        .to_socket_addrs()
        .map_err(|err| format!("could not resolve EggPool control endpoint: {err}"))?
        .next()
        .ok_or_else(|| "EggPool control endpoint resolved to no addresses".to_string())?;

    let ca_path = settings
        .ca
        .as_ref()
        .ok_or_else(|| "missing EggPool CA path".to_string())?;
    let ca_pem = std::fs::read(ca_path)
        .map_err(|err| format!("could not read the EggPool CA file: {err}"))?;
    let current_pin = ca_digest(&ca_pem)?;
    if !crate::keys::constant_time_eq(current_pin.as_bytes(), state.ca_sha256.as_bytes()) {
        return Err("EggPool CA certificate changed after ticket creation".to_string());
    }

    if !crate::config::is_loopback_host(local_host) {
        return Err("EggPool reverse target must be a loopback address".to_string());
    }
    let (auth_username, auth_password) = match (&state.enroll_token, state.expires_at) {
        (Some(token), Some(expires_at)) if expires_at >= crate::timeutil::now_unix() => {
            (Some("enroll".to_string()), Some(token.clone()))
        }
        (None, None) if state.client_cert_pem.is_some() && state.client_key_pem.is_some() => {
            (None, None)
        }
        _ => return Err("EggPool enrollment credential is incomplete or expired".to_string()),
    };
    let client_config = ReverseClientConfig {
        server_addr: address,
        default_target_host: Some(local_host.to_string()),
        default_target_port: Some(local_port),
        reconnect_initial_ms: 1_000,
        reconnect_max_ms: 30_000,
        tls: Some(ReverseClientTlsConfig {
            ca_pem: Some(ca_pem),
            server_name: settings
                .server_name
                .clone()
                .ok_or_else(|| "missing EggPool TLS server name".to_string())?,
            client_cert_pem: state
                .client_cert_pem
                .as_ref()
                .map(|cert| cert.as_bytes().to_vec()),
            client_key_pem: state
                .client_key_pem
                .as_ref()
                .map(|key| key.as_bytes().to_vec()),
        }),
        auth_username,
        auth_password,
        ..ReverseClientConfig::default()
    };
    client_config
        .validate()
        .map_err(|_| "EggPool reverse-client TLS configuration is invalid".to_string())?;
    Ok(client_config)
}

#[derive(Serialize, Deserialize)]
struct TunnelStatus {
    state: String,
    agent: String,
    control: String,
    updated_at: i64,
}

fn write_status(path: &Path, settings: &EggpoolSettings, state: &str) {
    let status = TunnelStatus {
        state: state.to_string(),
        agent: settings
            .label
            .clone()
            .unwrap_or_else(|| "eggpass".to_string()),
        control: settings
            .control
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        updated_at: crate::timeutil::now_unix(),
    };
    if let Ok(mut bytes) = serde_json::to_vec(&status) {
        let _ = write_private(path, &bytes);
        bytes.zeroize();
    }
}

fn validate_client_material(cert: &str, key: &str) -> Result<(), String> {
    if cert.is_empty()
        || cert.len() > MAX_CERT_PEM_BYTES
        || !cert.contains("-----BEGIN CERTIFICATE-----")
        || !cert.contains("-----END CERTIFICATE-----")
    {
        return Err("invalid EggPool client certificate".to_string());
    }
    let key_header = [
        "-----BEGIN PRIVATE KEY-----",
        "-----BEGIN EC PRIVATE KEY-----",
        "-----BEGIN RSA PRIVATE KEY-----",
    ];
    if key.is_empty()
        || key.len() > MAX_KEY_PEM_BYTES
        || key.contains("ENCRYPTED PRIVATE KEY")
        || !key_header.iter().any(|header| key.contains(header))
        || !key.contains("-----END ")
    {
        return Err("invalid EggPool client private key".to_string());
    }
    Ok(())
}

fn load_secrets(path: &Path) -> Result<Option<EggpoolSecrets>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("could not read EggPool private state: {err}")),
    };
    let result = serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "EggPool private state is invalid".to_string());
    let mut bytes = bytes;
    bytes.zeroize();
    result
}

fn write_secrets(path: &Path, state: &EggpoolSecrets) -> Result<(), String> {
    let bytes = Zeroizing::new(
        serde_json::to_vec_pretty(state)
            .map_err(|_| "could not encode EggPool private state".to_string())?,
    );
    write_private(path, bytes.as_slice())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|err| format!("could not create private state directory: {err}"))?;

    let mut random = [0u8; 8];
    rand::rng().fill_bytes(&mut random);
    let suffix = URL_SAFE_NO_PAD.encode(random);
    random.zeroize();
    let temporary = parent.join(format!(".eggpool-{suffix}.tmp"));

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|err| format!("could not create private state file: {err}"))?;
    if let Err(err) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("could not write private EggPool state: {err}"));
    }
    drop(file);
    if let Err(err) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("could not replace private EggPool state: {err}"));
    }
    restrict_permissions(path, 0o600)
        .map_err(|err| format!("could not restrict EggPool state permissions: {err}"))
}

fn remove_file_if_present(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("could not remove EggPool private state: {err}")),
    }
}

fn ca_digest(pem: &[u8]) -> Result<String, String> {
    let text =
        std::str::from_utf8(pem).map_err(|_| "EggPool CA file is not UTF-8 PEM".to_string())?;
    let begin = "-----BEGIN CERTIFICATE-----";
    let end = "-----END CERTIFICATE-----";
    let start = text
        .find(begin)
        .map(|index| index + begin.len())
        .ok_or_else(|| "EggPool CA file contains no PEM certificate".to_string())?;
    let finish = text[start..]
        .find(end)
        .map(|index| start + index)
        .ok_or_else(|| "EggPool CA PEM certificate is incomplete".to_string())?;
    let compact: Vec<u8> = text[start..finish]
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let der = STANDARD
        .decode(compact)
        .map_err(|_| "EggPool CA PEM certificate is malformed".to_string())?;
    if der.is_empty() {
        return Err("EggPool CA certificate is empty".to_string());
    }
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(der)))
}

pub(crate) fn validate_control(control: &str) -> Result<(), String> {
    if control.is_empty()
        || control.len() > 255
        || control.chars().any(char::is_whitespace)
        || control.contains('/')
    {
        return Err("invalid EggPool control endpoint".to_string());
    }
    let (host, port) = if control.starts_with('[') {
        let end = control
            .find(']')
            .ok_or_else(|| "invalid bracketed EggPool control address".to_string())?;
        let host = &control[1..end];
        let port = control[end + 1..]
            .strip_prefix(':')
            .ok_or_else(|| "EggPool control address needs a port".to_string())?;
        (host, port)
    } else {
        control
            .rsplit_once(':')
            .ok_or_else(|| "EggPool control address needs a port".to_string())?
    };
    let bracketed_ipv6 = control.starts_with('[');
    if host.is_empty()
        || (bracketed_ipv6 && host.parse::<std::net::Ipv6Addr>().is_err())
        || (!bracketed_ipv6 && (host.contains(':') || !valid_dns_host(host)))
    {
        return Err("invalid EggPool control host (IPv6 must be bracketed)".to_string());
    }
    let port: u16 = port
        .parse()
        .map_err(|_| "invalid EggPool control port".to_string())?;
    if port == 0 {
        return Err("EggPool control port must be between 1 and 65535".to_string());
    }
    Ok(())
}

fn valid_dns_host(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn validate_server_name(server_name: &str) -> Result<(), String> {
    if server_name.is_empty()
        || server_name.len() > 253
        || server_name.chars().any(char::is_whitespace)
        || server_name.contains('/')
    {
        return Err("invalid EggPool TLS server name".to_string());
    }
    Ok(())
}

fn validate_label(label: &str) -> Result<(), String> {
    if label.is_empty()
        || label.len() > 64
        || !label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("agent label must be 1-64 ASCII letters, digits, '.', '_' or '-'".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pem(bytes: &[u8]) -> Vec<u8> {
        format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            STANDARD.encode(bytes)
        )
        .into_bytes()
    }

    #[test]
    fn ca_digest_hashes_the_first_certificate_der_not_the_pem_text() {
        let digest = ca_digest(&pem(&[1, 2, 3, 4])).expect("digest");
        assert_eq!(digest, URL_SAFE_NO_PAD.encode(Sha256::digest([1, 2, 3, 4])));

        let bundle = [pem(&[1, 2, 3, 4]), pem(&[9, 8, 7])].concat();
        assert_eq!(ca_digest(&bundle).expect("bundle digest"), digest);
    }

    #[test]
    fn control_endpoint_requires_host_and_nonzero_port() {
        assert!(validate_control("eggpool.lan:18443").is_ok());
        assert!(validate_control("eggpool.lan.:18443").is_ok());
        assert!(validate_control("127.0.0.1:18443").is_ok());
        assert!(validate_control("[::1]:18443").is_ok());
        assert!(validate_control("eggpool.lan:0").is_err());
        assert!(validate_control("https://eggpool.lan:18443").is_err());
        assert!(validate_control("user@eggpool.lan:18443").is_err());
        assert!(validate_control("pool.example?token=x:18443").is_err());
    }

    #[test]
    fn ticket_is_single_line_and_secrets_stay_out_of_config_and_key_store() {
        let directory = tempfile::tempdir().expect("temp dir");
        let paths = Paths::in_directory(directory.path());
        crate::config::ensure_config_file(paths.config_file()).expect("default config");
        let ca_path = directory.path().join("ca.pem");
        std::fs::write(&ca_path, pem(&[1, 2, 3, 4])).expect("write CA fixture");

        let ticket = create_ticket(
            &paths,
            &Config::default(),
            TicketSetup {
                control: "pool.example:18443",
                server_name: "pool.example",
                ca_path: &ca_path,
                label: "test-agent",
                live_host: "127.0.0.1",
                live_port: 8484,
            },
        )
        .expect("create ticket");
        let line = ticket.as_str();
        assert!(line.starts_with("epagent1."));
        assert!(line.is_ascii() && !line.chars().any(char::is_whitespace));
        assert!(line.len() <= MAX_TICKET_BYTES);

        let encoded = line.strip_prefix("epagent1.").expect("ticket prefix");
        let payload = URL_SAFE_NO_PAD.decode(encoded).expect("ticket base64");
        let parsed: serde_json::Value = serde_json::from_slice(&payload).expect("ticket JSON");
        assert_eq!(parsed.as_object().map(serde_json::Map::len), Some(9));
        assert_eq!(parsed["v"], 1);
        assert_eq!(parsed["control"], "pool.example:18443");
        assert_eq!(parsed["server_name"], "pool.example");
        assert_eq!(parsed["agent"], "test-agent");
        assert_eq!(parsed["eggpass"], env!("CARGO_PKG_VERSION"));
        let expected_pin = URL_SAFE_NO_PAD.encode(Sha256::digest([1, 2, 3, 4]));
        assert_eq!(parsed["ca_sha256"].as_str(), Some(expected_pin.as_str()));
        let expiry = parsed["expires_at"].as_i64().expect("expiry");
        let remaining = expiry - crate::timeutil::now_unix();
        assert!((599..=600).contains(&remaining));
        let enroll = parsed["enroll"].as_str().expect("enrollment token");
        let api_key = parsed["api_key"].as_str().expect("EggPass API key");
        assert_ne!(
            enroll, api_key,
            "bootstrap and inference credentials are distinct"
        );

        let secret_text =
            std::fs::read_to_string(paths.eggpool_secret_file()).expect("private state");
        let key_text = std::fs::read_to_string(paths.key_store_file()).expect("key store");
        let config_text = std::fs::read_to_string(paths.config_file()).expect("config");
        assert!(secret_text.contains(enroll));
        assert!(!secret_text.contains(api_key));
        assert!(!key_text.contains(api_key));
        assert!(!config_text.contains(enroll));
        assert!(!config_text.contains(api_key));
        assert!(config_text.contains("enabled = true"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(paths.eggpool_secret_file())
                .expect("private state metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn invalid_mtls_material_does_not_consume_the_enrollment_token() {
        let directory = tempfile::tempdir().expect("temp dir");
        let ca = pem(&[1, 2, 3, 4]);
        let path = directory.path().join("eggpool.json");
        let mut state = EggpoolSecrets {
            version: 1,
            agent: "test-agent".to_string(),
            api_key_id: 1,
            ca_sha256: ca_digest(&ca).expect("pin"),
            enroll_token: Some("one-use".to_string()),
            expires_at: Some(crate::timeutil::now_unix() + 600),
            client_cert_pem: None,
            client_key_pem: None,
        };
        write_secrets(&path, &state).expect("write state");
        state.enroll_token = None;
        let store = EnrollmentStore::new(
            path.clone(),
            ca,
            "eggpool.lan".to_string(),
            Arc::new(Notify::new()),
        );
        assert!(store.authorizes("one-use"));
        let invalid = br#"{"client_cert_pem":"bad","client_key_pem":"bad"}"#;
        assert!(store.complete("one-use", invalid).is_err());
        assert!(store.authorizes("one-use"));
    }

    #[test]
    fn bearer_parser_accepts_only_a_single_bearer_value() {
        assert_eq!(enrollment_bearer(Some("Bearer abc123")), Some("abc123"));
        assert_eq!(enrollment_bearer(Some("Basic abc123")), None);
        assert_eq!(enrollment_bearer(Some("Bearer abc 123")), None);
    }
}
