use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eggpass_auth::{
    CancellationToken, ClientIdPolicy, ClientOptions, Credentials, DeviceCode, DevicePoll, Error,
    OAuthClient, OAuthProviderConfig, PendingAuthorization, TokenSet, UserInfo,
};
use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::PyModule;
use pyo3_async_runtimes::tokio::future_into_py;

create_exception!(_native, EggpassAuthError, PyException);
create_exception!(_native, InvalidConfigurationError, EggpassAuthError);
create_exception!(_native, InvalidResponseError, EggpassAuthError);
create_exception!(_native, HttpStatusError, EggpassAuthError);
create_exception!(_native, OAuthError, EggpassAuthError);
create_exception!(_native, ProviderDeniedError, EggpassAuthError);
create_exception!(_native, CallbackMismatchError, EggpassAuthError);
create_exception!(_native, StateMismatchError, EggpassAuthError);
create_exception!(_native, NonceMismatchError, EggpassAuthError);
create_exception!(_native, ResponseTooLargeError, EggpassAuthError);
create_exception!(_native, TransportError, EggpassAuthError);
create_exception!(_native, OperationTimeoutError, EggpassAuthError);
create_exception!(_native, UnsupportedOperationError, EggpassAuthError);
create_exception!(_native, DeviceExpiredError, EggpassAuthError);
create_exception!(_native, OperationCancelledError, EggpassAuthError);
create_exception!(_native, UntrustedVerificationUrlError, EggpassAuthError);
create_exception!(_native, NotLoggedInError, EggpassAuthError);
create_exception!(_native, NoRefreshTokenError, EggpassAuthError);
create_exception!(_native, CredentialStorageError, EggpassAuthError);

fn map_error(error: Error) -> PyErr {
    match error {
        Error::InvalidResponse(message) => InvalidResponseError::new_err(message),
        Error::InvalidConfiguration(message) => InvalidConfigurationError::new_err(message),
        Error::HttpStatus { status } => {
            let py_error = HttpStatusError::new_err(format!("HTTP {status} from OAuth endpoint"));
            Python::attach(|py| {
                let _ = py_error.value(py).setattr("status", status);
            });
            py_error
        }
        Error::OAuth { status, code } => {
            let py_error =
                OAuthError::new_err(format!("OAuth endpoint returned HTTP {status} ({code})"));
            Python::attach(|py| {
                let value = py_error.value(py);
                let _ = value.setattr("status", status);
                let _ = value.setattr("code", code);
            });
            py_error
        }
        Error::ProviderDenied(code) => {
            let py_error =
                ProviderDeniedError::new_err(format!("provider denied authorization ({code})"));
            Python::attach(|py| {
                let _ = py_error.value(py).setattr("code", code);
            });
            py_error
        }
        Error::CallbackMismatch => {
            CallbackMismatchError::new_err("callback did not match the pending authorization")
        }
        Error::StateMismatch => {
            StateMismatchError::new_err("callback state did not match the pending authorization")
        }
        Error::NonceMismatch => {
            NonceMismatchError::new_err("ID-token nonce did not match the pending authorization")
        }
        Error::ResponseTooLarge => {
            ResponseTooLargeError::new_err("OAuth response exceeded the configured body limit")
        }
        Error::Transport => TransportError::new_err("OAuth HTTP request failed"),
        Error::Timeout(operation) => {
            OperationTimeoutError::new_err(format!("OAuth {operation} timed out"))
        }
        Error::Unsupported(operation) => {
            UnsupportedOperationError::new_err(format!("provider does not support {operation}"))
        }
        Error::DeviceExpired => DeviceExpiredError::new_err("device authorization expired"),
        Error::Cancelled => OperationCancelledError::new_err("device authorization was cancelled"),
        Error::UntrustedVerificationUrl { .. } => UntrustedVerificationUrlError::new_err(
            "device verification URL was rejected by the trusted-host policy",
        ),
        Error::NotLoggedIn => NotLoggedInError::new_err("no credentials are stored"),
        Error::NoRefreshToken => {
            NoRefreshTokenError::new_err("stored credentials contain no refresh token")
        }
        Error::Storage(_) => CredentialStorageError::new_err("credential storage failed"),
        _ => EggpassAuthError::new_err("OAuth operation failed"),
    }
}

#[pyclass(name = "ProviderProfile", module = "eggpass_auth._native")]
struct PyProviderProfile {
    inner: OAuthProviderConfig,
}

#[pymethods]
impl PyProviderProfile {
    #[staticmethod]
    fn xai() -> Self {
        Self {
            inner: OAuthProviderConfig::xai(),
        }
    }

    #[staticmethod]
    fn xai_compatibility_fallback() -> Self {
        Self {
            inner: OAuthProviderConfig::xai_compatibility_fallback(),
        }
    }

    #[staticmethod]
    #[pyo3(signature = (host_id=None))]
    fn openai(host_id: Option<String>) -> PyResult<Self> {
        let inner = match host_id {
            Some(host_id) => OAuthProviderConfig::openai_with_host_id(host_id),
            None => Ok(OAuthProviderConfig::openai()),
        }
        .map_err(map_error)?;
        Ok(Self { inner })
    }

    #[staticmethod]
    fn custom(display_name: String, issuer: String, client_id: String) -> PyProviderBuilder {
        PyProviderBuilder {
            display_name,
            issuer,
            client_id,
            authorization_endpoint: None,
            token_endpoint: None,
            device_authorization_endpoint: None,
            userinfo_endpoint: None,
            revocation_endpoint: None,
            callback_path: "/callback".to_string(),
            scopes: Vec::new(),
            client_id_policy: ClientIdPolicy::Fixed,
            authorization_parameters: Vec::new(),
            registration_parameters: Vec::new(),
            token_parameters: Vec::new(),
            device_parameters: Vec::new(),
            trusted_device_hosts: Vec::new(),
        }
    }

    #[getter]
    fn display_name(&self) -> String {
        self.inner.display_name().to_string()
    }

    #[getter]
    fn issuer(&self) -> String {
        self.inner.issuer().to_string()
    }

    #[getter]
    fn callback_path(&self) -> String {
        self.inner.callback_path().to_string()
    }

    #[getter]
    fn scopes(&self) -> Vec<String> {
        self.inner.scopes().to_vec()
    }

    #[getter]
    fn client_id_policy(&self) -> &'static str {
        match self.inner.client_id_policy() {
            ClientIdPolicy::Fixed => "fixed",
            ClientIdPolicy::IssuedOnRegistration => "issued_on_registration",
            _ => "unknown",
        }
    }

    fn __repr__(&self) -> &'static str {
        "ProviderProfile(<configuration hidden>)"
    }
}

#[pyclass(name = "ProviderBuilder", module = "eggpass_auth._native")]
struct PyProviderBuilder {
    display_name: String,
    issuer: String,
    client_id: String,
    authorization_endpoint: Option<String>,
    token_endpoint: Option<String>,
    device_authorization_endpoint: Option<String>,
    userinfo_endpoint: Option<String>,
    revocation_endpoint: Option<String>,
    callback_path: String,
    scopes: Vec<String>,
    client_id_policy: ClientIdPolicy,
    authorization_parameters: Vec<(String, String)>,
    registration_parameters: Vec<(String, String)>,
    token_parameters: Vec<(String, String)>,
    device_parameters: Vec<(String, String)>,
    trusted_device_hosts: Vec<String>,
}

#[pymethods]
impl PyProviderBuilder {
    fn authorization_endpoint(&mut self, value: String) {
        self.authorization_endpoint = Some(value);
    }

    fn token_endpoint(&mut self, value: String) {
        self.token_endpoint = Some(value);
    }

    fn device_authorization_endpoint(&mut self, value: String) {
        self.device_authorization_endpoint = Some(value);
    }

    fn userinfo_endpoint(&mut self, value: String) {
        self.userinfo_endpoint = Some(value);
    }

    fn revocation_endpoint(&mut self, value: String) {
        self.revocation_endpoint = Some(value);
    }

    fn callback_path(&mut self, value: String) {
        self.callback_path = value;
    }

    fn scopes(&mut self, values: Vec<String>) {
        self.scopes = values;
    }

    fn client_id_policy(&mut self, value: &str) -> PyResult<()> {
        self.client_id_policy = match value {
            "fixed" => ClientIdPolicy::Fixed,
            "issued_on_registration" => ClientIdPolicy::IssuedOnRegistration,
            _ => {
                return Err(InvalidConfigurationError::new_err(
                    "client_id_policy must be 'fixed' or 'issued_on_registration'",
                ));
            }
        };
        Ok(())
    }

    fn authorization_parameter(&mut self, name: String, value: String) {
        self.authorization_parameters.push((name, value));
    }

    fn registration_parameter(&mut self, name: String, value: String) {
        self.registration_parameters.push((name, value));
    }

    fn token_parameter(&mut self, name: String, value: String) {
        self.token_parameters.push((name, value));
    }

    fn device_parameter(&mut self, name: String, value: String) {
        self.device_parameters.push((name, value));
    }

    fn trusted_device_hosts(&mut self, hosts: Vec<String>) {
        self.trusted_device_hosts = hosts;
    }

    fn build(&self) -> PyResult<PyProviderProfile> {
        let mut builder = OAuthProviderConfig::custom(
            self.display_name.clone(),
            self.issuer.clone(),
            self.client_id.clone(),
        )
        .callback_path(self.callback_path.clone())
        .scopes(self.scopes.clone())
        .client_id_policy(self.client_id_policy);

        if let Some(value) = &self.authorization_endpoint {
            builder = builder.authorization_endpoint(value.clone());
        }
        if let Some(value) = &self.token_endpoint {
            builder = builder.token_endpoint(value.clone());
        }
        if let Some(value) = &self.device_authorization_endpoint {
            builder = builder.device_authorization_endpoint(value.clone());
        }
        if let Some(value) = &self.userinfo_endpoint {
            builder = builder.userinfo_endpoint(value.clone());
        }
        if let Some(value) = &self.revocation_endpoint {
            builder = builder.revocation_endpoint(value.clone());
        }
        for (name, value) in &self.authorization_parameters {
            builder = builder.authorization_parameter(name.clone(), value.clone());
        }
        for (name, value) in &self.registration_parameters {
            builder = builder.registration_parameter(name.clone(), value.clone());
        }
        for (name, value) in &self.token_parameters {
            builder = builder.token_parameter(name.clone(), value.clone());
        }
        for (name, value) in &self.device_parameters {
            builder = builder.device_parameter(name.clone(), value.clone());
        }
        for host in &self.trusted_device_hosts {
            builder = builder.trusted_device_host(host.clone());
        }
        let inner = builder.build().map_err(map_error)?;
        Ok(PyProviderProfile { inner })
    }

    fn __repr__(&self) -> &'static str {
        "ProviderBuilder(<configuration hidden>)"
    }
}

#[pyclass(name = "OAuthClient", module = "eggpass_auth._native")]
struct PyOAuthClient {
    inner: OAuthClient,
}

#[pymethods]
impl PyOAuthClient {
    #[new]
    #[pyo3(signature = (profile, timeout_seconds=60, max_response_body_bytes=1048576))]
    fn new(
        profile: PyRef<'_, PyProviderProfile>,
        timeout_seconds: u64,
        max_response_body_bytes: usize,
    ) -> PyResult<Self> {
        if timeout_seconds == 0 {
            return Err(InvalidConfigurationError::new_err(
                "timeout_seconds must be greater than zero",
            ));
        }
        let timeout = Duration::from_secs(timeout_seconds);
        let mut options = ClientOptions::default();
        options.timeout.pool = Some(timeout);
        options.timeout.connect = Some(timeout);
        options.timeout.write = Some(timeout);
        options.timeout.read = Some(timeout);
        options.timeout.total = Some(timeout);
        options.max_response_body_bytes = max_response_body_bytes;
        let inner = OAuthClient::new(eggpass_auth::EggfetchClient::new(), profile.inner.clone())
            .with_options(options)
            .map_err(map_error)?;
        Ok(Self { inner })
    }

    #[getter]
    fn provider(&self, py: Python<'_>) -> PyResult<Py<PyProviderProfile>> {
        Py::new(
            py,
            PyProviderProfile {
                inner: self.inner.provider().clone(),
            },
        )
    }

    #[pyo3(signature = (redirect_uri, existing=None))]
    fn begin_authorization(
        &self,
        py: Python<'_>,
        redirect_uri: String,
        existing: Option<PyRef<'_, PyCredentials>>,
    ) -> PyResult<Py<PyPendingAuthorization>> {
        let existing = existing.map(|credentials| credentials.inner.clone());
        let pending = self
            .inner
            .begin_authorization(&redirect_uri, existing.as_ref())
            .map_err(map_error)?;
        Py::new(
            py,
            PyPendingAuthorization {
                inner: Some(pending),
            },
        )
    }

    fn complete_authorization<'py>(
        &self,
        py: Python<'py>,
        mut pending: PyRefMut<'_, PyPendingAuthorization>,
        callback_url: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let pending = pending.inner.take().ok_or_else(|| {
            InvalidConfigurationError::new_err("pending authorization has already been consumed")
        })?;
        let client = self.inner.clone();
        future_into_py(py, async move {
            let credentials = client
                .complete_authorization(pending, &callback_url)
                .await
                .map_err(map_error)?;
            Python::attach(|py| Py::new(py, PyCredentials { inner: credentials }))
        })
    }

    fn complete_authorization_with_code<'py>(
        &self,
        py: Python<'py>,
        mut pending: PyRefMut<'_, PyPendingAuthorization>,
        code: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let pending = pending.inner.take().ok_or_else(|| {
            InvalidConfigurationError::new_err("pending authorization has already been consumed")
        })?;
        let client = self.inner.clone();
        future_into_py(py, async move {
            let credentials = client
                .complete_authorization_with_code(pending, &code)
                .await
                .map_err(map_error)?;
            Python::attach(|py| Py::new(py, PyCredentials { inner: credentials }))
        })
    }

    #[pyo3(signature = (code, redirect_uri, code_verifier, client_id=None))]
    fn exchange_authorization_code<'py>(
        &self,
        py: Python<'py>,
        code: String,
        redirect_uri: String,
        code_verifier: String,
        client_id: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.inner.clone();
        future_into_py(py, async move {
            let credentials = client
                .exchange_authorization_code(
                    &code,
                    &redirect_uri,
                    &code_verifier,
                    client_id.as_deref(),
                )
                .await
                .map_err(map_error)?;
            Python::attach(|py| Py::new(py, PyCredentials { inner: credentials }))
        })
    }

    #[pyo3(signature = (refresh_token, client_id=None))]
    fn refresh_token<'py>(
        &self,
        py: Python<'py>,
        refresh_token: String,
        client_id: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.inner.clone();
        future_into_py(py, async move {
            let tokens = client
                .refresh_token(&refresh_token, client_id.as_deref())
                .await
                .map_err(map_error)?;
            Python::attach(|py| Py::new(py, PyTokenSet { inner: tokens }))
        })
    }

    fn refresh_credentials<'py>(
        &self,
        py: Python<'py>,
        credentials: PyRef<'_, PyCredentials>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.inner.clone();
        let current = credentials.inner.clone();
        future_into_py(py, async move {
            let credentials = client
                .refresh_credentials(&current)
                .await
                .map_err(map_error)?;
            Python::attach(|py| Py::new(py, PyCredentials { inner: credentials }))
        })
    }

    #[pyo3(signature = (token, client_id=None))]
    fn revoke<'py>(
        &self,
        py: Python<'py>,
        token: String,
        client_id: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.inner.clone();
        future_into_py(py, async move {
            client
                .revoke(&token, client_id.as_deref())
                .await
                .map_err(map_error)?;
            Ok(())
        })
    }

    fn userinfo<'py>(&self, py: Python<'py>, access_token: String) -> PyResult<Bound<'py, PyAny>> {
        let client = self.inner.clone();
        future_into_py(py, async move {
            let info = client.userinfo(&access_token).await.map_err(map_error)?;
            Python::attach(|py| Py::new(py, PyUserInfo { inner: info }))
        })
    }

    fn request_device_code<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let client = self.inner.clone();
        future_into_py(py, async move {
            let device = client.request_device_code().await.map_err(map_error)?;
            Python::attach(|py| Py::new(py, PyDeviceCode { inner: device }))
        })
    }

    fn poll_device_code<'py>(
        &self,
        py: Python<'py>,
        device: PyRef<'_, PyDeviceCode>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.inner.clone();
        let device_code = device.inner.device_code().to_string();
        future_into_py(py, async move {
            let poll = client
                .poll_device_code(&device_code)
                .await
                .map_err(map_error)?;
            Python::attach(|py| Py::new(py, PyDevicePoll { inner: poll }))
        })
    }

    #[pyo3(signature = (device, cancellation=None, timeout_seconds=600))]
    fn poll_device_authorization<'py>(
        &self,
        py: Python<'py>,
        device: PyRef<'_, PyDeviceCode>,
        cancellation: Option<PyRef<'_, PyCancellationToken>>,
        timeout_seconds: u64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.inner.clone();
        let device = device.inner.clone();
        let cancellation = cancellation
            .map(|token| token.inner.clone())
            .unwrap_or_default();
        future_into_py(py, async move {
            let credentials = client
                .poll_device_authorization(
                    &device,
                    &cancellation,
                    Duration::from_secs(timeout_seconds),
                )
                .await
                .map_err(map_error)?;
            Python::attach(|py| Py::new(py, PyCredentials { inner: credentials }))
        })
    }

    fn __repr__(&self) -> &'static str {
        "OAuthClient(<transport settings hidden>)"
    }
}

#[pyclass(name = "PendingAuthorization", module = "eggpass_auth._native")]
struct PyPendingAuthorization {
    inner: Option<PendingAuthorization>,
}

#[pymethods]
impl PyPendingAuthorization {
    #[getter]
    fn authorization_url(&self) -> PyResult<String> {
        self.inner
            .as_ref()
            .map(|pending| pending.authorization_url().to_string())
            .ok_or_else(|| {
                InvalidConfigurationError::new_err(
                    "pending authorization has already been consumed",
                )
            })
    }

    #[getter]
    fn consumed(&self) -> bool {
        self.inner.is_none()
    }

    fn __repr__(&self) -> &'static str {
        "PendingAuthorization(<opaque; sensitive values redacted>)"
    }
}

#[pyclass(name = "Credentials", module = "eggpass_auth._native")]
struct PyCredentials {
    inner: Credentials,
}

#[pymethods]
impl PyCredentials {
    #[getter]
    fn access_token(&self) -> String {
        self.inner.access_token().to_string()
    }

    #[getter]
    fn refresh_token(&self) -> Option<String> {
        self.inner.refresh_token().map(str::to_string)
    }

    #[getter]
    fn id_token(&self) -> Option<String> {
        self.inner.id_token().map(str::to_string)
    }

    #[getter]
    fn token_type(&self) -> String {
        self.inner.token_type().to_string()
    }

    #[getter]
    fn scope(&self) -> Option<String> {
        self.inner.scope().map(str::to_string)
    }

    #[getter]
    fn client_id(&self) -> String {
        self.inner.client_id().to_string()
    }

    #[getter]
    fn issuer(&self) -> String {
        self.inner.issuer().to_string()
    }

    #[getter]
    fn expires_at(&self) -> Option<i64> {
        self.inner.expires_at()
    }

    #[getter]
    fn obtained_at(&self) -> i64 {
        self.inner.obtained_at()
    }

    #[pyo3(signature = (now_unix=None, skew_seconds=0))]
    fn is_fresh(&self, now_unix: Option<i64>, skew_seconds: i64) -> bool {
        let now = now_unix.unwrap_or_else(now_unix_seconds);
        self.inner.is_fresh(now, skew_seconds)
    }

    /// Serialize secret-bearing credentials explicitly for caller-owned storage.
    fn to_json(&self) -> PyResult<String> {
        serde_json::to_string(&self.inner)
            .map_err(|_| InvalidResponseError::new_err("credentials could not be serialized"))
    }

    /// Load secret-bearing credentials from caller-owned serialized storage.
    #[staticmethod]
    fn from_json(serialized: &str) -> PyResult<Self> {
        let inner = serde_json::from_str(serialized)
            .map_err(|_| InvalidResponseError::new_err("credential JSON is invalid"))?;
        Ok(Self { inner })
    }

    fn __repr__(&self) -> &'static str {
        "Credentials(<secrets redacted>)"
    }
}

#[pyclass(name = "TokenSet", module = "eggpass_auth._native")]
struct PyTokenSet {
    inner: TokenSet,
}

#[pymethods]
impl PyTokenSet {
    #[getter]
    fn access_token(&self) -> String {
        self.inner.access_token().to_string()
    }

    #[getter]
    fn refresh_token(&self) -> Option<String> {
        self.inner.refresh_token().map(str::to_string)
    }

    #[getter]
    fn id_token(&self) -> Option<String> {
        self.inner.id_token().map(str::to_string)
    }

    #[getter]
    fn token_type(&self) -> String {
        self.inner.token_type().to_string()
    }

    #[getter]
    fn scope(&self) -> Option<String> {
        self.inner.scope().map(str::to_string)
    }

    #[getter]
    fn expires_at(&self) -> Option<i64> {
        self.inner.expires_at()
    }

    fn __repr__(&self) -> &'static str {
        "TokenSet(<secrets redacted>)"
    }
}

#[pyclass(name = "DeviceCode", module = "eggpass_auth._native")]
struct PyDeviceCode {
    inner: DeviceCode,
}

#[pymethods]
impl PyDeviceCode {
    #[getter]
    fn user_code(&self) -> String {
        self.inner.user_code().to_string()
    }

    #[getter]
    fn verification_uri(&self) -> String {
        self.inner.verification_uri().to_string()
    }

    #[getter]
    fn verification_uri_complete(&self) -> Option<String> {
        self.inner.verification_uri_complete().map(str::to_string)
    }

    #[getter]
    fn expires_in_seconds(&self) -> f64 {
        self.inner.expires_in().as_secs_f64()
    }

    #[getter]
    fn interval_seconds(&self) -> f64 {
        self.inner.interval().as_secs_f64()
    }

    fn __repr__(&self) -> String {
        format!(
            "DeviceCode(user_code=<redacted>, verification_uri=<redacted>, expires_in_seconds={}, interval_seconds={})",
            self.inner.expires_in().as_secs_f64(),
            self.inner.interval().as_secs_f64()
        )
    }
}

#[pyclass(name = "DevicePoll", module = "eggpass_auth._native")]
struct PyDevicePoll {
    inner: DevicePoll,
}

#[pymethods]
impl PyDevicePoll {
    #[getter]
    fn status(&self) -> &'static str {
        match &self.inner {
            DevicePoll::Pending => "pending",
            DevicePoll::SlowDown => "slow_down",
            DevicePoll::Denied => "denied",
            DevicePoll::Expired => "expired",
            DevicePoll::Complete(_) => "complete",
            _ => "unknown",
        }
    }

    #[getter]
    fn tokens(&self, py: Python<'_>) -> PyResult<Option<Py<PyTokenSet>>> {
        match &self.inner {
            DevicePoll::Complete(tokens) => Ok(Some(Py::new(
                py,
                PyTokenSet {
                    inner: tokens.clone(),
                },
            )?)),
            _ => Ok(None),
        }
    }

    fn __repr__(&self) -> String {
        format!("DevicePoll(status={:?})", self.status())
    }
}

#[pyclass(name = "UserInfo", module = "eggpass_auth._native")]
struct PyUserInfo {
    inner: UserInfo,
}

#[pymethods]
impl PyUserInfo {
    #[getter]
    fn user_id(&self) -> Option<String> {
        self.inner.user_id.clone()
    }

    #[getter]
    fn email(&self) -> Option<String> {
        self.inner.email.clone()
    }

    fn __repr__(&self) -> &'static str {
        "UserInfo(<account details omitted>)"
    }
}

#[pyclass(name = "CancellationToken", module = "eggpass_auth._native")]
struct PyCancellationToken {
    inner: CancellationToken,
}

#[pymethods]
impl PyCancellationToken {
    #[new]
    fn new() -> Self {
        Self {
            inner: CancellationToken::new(),
        }
    }

    fn cancel(&self) {
        self.inner.cancel();
    }

    #[getter]
    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn __repr__(&self) -> &'static str {
        "CancellationToken(<state omitted>)"
    }
}

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyProviderProfile>()?;
    module.add_class::<PyProviderBuilder>()?;
    module.add_class::<PyOAuthClient>()?;
    module.add_class::<PyPendingAuthorization>()?;
    module.add_class::<PyCredentials>()?;
    module.add_class::<PyTokenSet>()?;
    module.add_class::<PyDeviceCode>()?;
    module.add_class::<PyDevicePoll>()?;
    module.add_class::<PyUserInfo>()?;
    module.add_class::<PyCancellationToken>()?;

    let py = module.py();
    module.add("EggpassAuthError", py.get_type::<EggpassAuthError>())?;
    module.add(
        "InvalidConfigurationError",
        py.get_type::<InvalidConfigurationError>(),
    )?;
    module.add(
        "InvalidResponseError",
        py.get_type::<InvalidResponseError>(),
    )?;
    module.add("HttpStatusError", py.get_type::<HttpStatusError>())?;
    module.add("OAuthError", py.get_type::<OAuthError>())?;
    module.add("ProviderDeniedError", py.get_type::<ProviderDeniedError>())?;
    module.add(
        "CallbackMismatchError",
        py.get_type::<CallbackMismatchError>(),
    )?;
    module.add("StateMismatchError", py.get_type::<StateMismatchError>())?;
    module.add("NonceMismatchError", py.get_type::<NonceMismatchError>())?;
    module.add(
        "ResponseTooLargeError",
        py.get_type::<ResponseTooLargeError>(),
    )?;
    module.add("TransportError", py.get_type::<TransportError>())?;
    module.add(
        "OperationTimeoutError",
        py.get_type::<OperationTimeoutError>(),
    )?;
    module.add(
        "UnsupportedOperationError",
        py.get_type::<UnsupportedOperationError>(),
    )?;
    module.add("DeviceExpiredError", py.get_type::<DeviceExpiredError>())?;
    module.add(
        "OperationCancelledError",
        py.get_type::<OperationCancelledError>(),
    )?;
    module.add(
        "UntrustedVerificationUrlError",
        py.get_type::<UntrustedVerificationUrlError>(),
    )?;
    module.add("NotLoggedInError", py.get_type::<NotLoggedInError>())?;
    module.add("NoRefreshTokenError", py.get_type::<NoRefreshTokenError>())?;
    module.add(
        "CredentialStorageError",
        py.get_type::<CredentialStorageError>(),
    )?;
    Ok(())
}
