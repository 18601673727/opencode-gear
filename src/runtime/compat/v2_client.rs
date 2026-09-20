//! OpenCode 2.x local session transport.
//!
//! [`V2SessionClient`] implements the version-agnostic
//! [`SessionClient`](super::SessionClient) contract against the OpenCode 2
//! local daemon. Every HTTP route, JSON shape and discovery detail lives here,
//! so no other module has to know how the v2 runtime is reached and no Lead
//! policy is duplicated: [`select_session_lead`](super::select_session_lead)
//! still decides *which* agent/model/variant to apply.
//!
//! The routes below were read from the running OpenCode 2.0.x server's own
//! OpenAPI document (`GET /openapi.json`) and exercised live; they are not
//! inferred:
//!
//! ```text
//! POST /api/session                          {location:{directory}}          -> {data:{id}}
//! GET  /api/session?directory=&limit=1       (newest root sessions first)   -> {data:[{id},...], cursor}
//! POST /api/session/{id}/agent               {agent}                        -> 204
//! POST /api/session/{id}/model               {model:{id, providerID[, variant]}} -> 204
//! GET  /api/session/{id}                     -> {data:{agent, model:{id, providerID[, variant]}}}
//! ```
//!
//! The variant key is written **only** when the Rust-resolved contract carries
//! one; a `None` variant omits it completely rather than sending `null`, an
//! empty string or a fabricated default.
//!
//! Discovery can use OpenCode's own background-service registration at
//! `$XDG_STATE_HOME/opencode/service.json` (default
//! `~/.local/state/opencode/service.json`), which carries the loopback URL and
//! the local Basic-auth password. That password is a local service credential:
//! it is held in a [`Secret`] and never rendered, logged or attached to an
//! error. Gear never reads OpenCode's provider credential store (`auth.json`).
//!
//! Production launch creates an invocation-scoped `opencode serve` child and
//! passes its ephemeral registration directly to this client. Discovery remains
//! available for diagnostics, but production never restarts or reconfigures a
//! user's background service.

use crate::error::{GearError, Result};
use crate::http::Secret;
use crate::runtime::compat::{EffectiveLead, SessionClient};
use serde_json::{json, Map, Value};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The username OpenCode's local daemon expects for its Basic-auth challenge.
/// Only the password is per-service; the username is fixed by the runtime.
const LOCAL_SERVICE_USERNAME: &str = "opencode";

/// The default request timeout for a local daemon call.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A running OpenCode 2 background service, as registered by OpenCode itself.
///
/// The registration file is OpenCode's supported discovery mechanism and also
/// carries the loopback Basic-auth password. That password is a credential and
/// is kept in a [`Secret`]: it can only be reached through
/// [`ServiceRegistration::password`] and is redacted from every rendering.
#[derive(Clone)]
pub struct ServiceRegistration {
    url: String,
    password: Secret,
}

impl ServiceRegistration {
    pub fn new(url: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            url: normalize_base_url(url.into()),
            password: Secret::new(password),
        }
    }

    /// The loopback base URL (no trailing slash).
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The local service password. Never log, print or persist the result.
    pub fn password(&self) -> &Secret {
        &self.password
    }

    /// Parse a registration document. Missing or empty `url`/`password` is a
    /// deterministic error; the raw document is never echoed.
    pub fn from_json(text: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(text).map_err(|error| {
            GearError::config(format!(
                "OpenCode service registration is not valid JSON: {error}"
            ))
        })?;
        let url = value
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let password = value.get("password").and_then(Value::as_str).unwrap_or("");
        if url.is_empty() || password.is_empty() {
            return Err(GearError::config(
                "OpenCode service registration is missing the local service url or password",
            ));
        }
        Ok(Self::new(url, password))
    }

    /// Read a registration file.
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|error| GearError::read(path, error))?;
        Self::from_json(&text)
    }

    /// Discover the running OpenCode 2 service through its own registration
    /// file. A missing file means "no service is running", not a parse problem.
    pub fn discover() -> Result<Self> {
        let base = directories::BaseDirs::new().ok_or_else(|| {
            GearError::config("cannot determine the home directory for OpenCode service discovery")
        })?;
        let state_home = base.state_dir().map(Path::to_path_buf);
        let path = service_registration_path(state_home.as_deref(), base.home_dir());
        if !path.is_file() {
            return Err(GearError::config(format!(
                "no running OpenCode V2 service registration was found at {}; start it with `opencode service start`",
                path.display()
            )));
        }
        Self::from_file(&path)
    }
}

impl fmt::Debug for ServiceRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServiceRegistration")
            .field("url", &self.url)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Resolve the registration path from an optional state directory and a home
/// directory. Pure so the layout is tested without touching the environment.
fn service_registration_path(state_home: Option<&Path>, home: &Path) -> PathBuf {
    let state = state_home
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".local").join("state"));
    state.join("opencode").join("service.json")
}

fn normalize_base_url(url: String) -> String {
    url.trim().trim_end_matches('/').to_string()
}

/// One request against the local daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2Request {
    pub method: &'static str,
    pub url: String,
    /// `None` sends an empty body; the body never contains credentials.
    pub body: Option<Value>,
}

/// One raw response from the local daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2Response {
    pub status: u16,
    pub body: String,
}

/// The transport seam. Production binds [`ReqwestV2Transport`]; tests bind a
/// deterministic in-memory fake, exactly like the other runtime boundaries.
pub trait V2Transport: Send + Sync {
    fn send(&self, request: V2Request) -> Result<V2Response>;
}

/// The production transport: a blocking `reqwest` client that authenticates
/// with the local service password and never uses an ambient proxy.
pub struct ReqwestV2Transport {
    client: reqwest::blocking::Client,
    password: Secret,
}

impl ReqwestV2Transport {
    pub fn new(registration: &ServiceRegistration) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("opencode-gear/", env!("CARGO_PKG_VERSION")))
            .timeout(REQUEST_TIMEOUT)
            .no_proxy()
            .build()
            .map_err(|error| {
                GearError::config(format!(
                    "cannot build the OpenCode V2 session client: {error}"
                ))
            })?;
        Ok(Self {
            client,
            password: registration.password().clone(),
        })
    }
}

impl V2Transport for ReqwestV2Transport {
    fn send(&self, request: V2Request) -> Result<V2Response> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|_| {
            GearError::config(format!("unsupported HTTP method {}", request.method))
        })?;
        let mut builder = self
            .client
            .request(method, &request.url)
            .basic_auth(LOCAL_SERVICE_USERNAME, Some(self.password.expose()));
        if let Some(body) = &request.body {
            let encoded = serde_json::to_string(body).map_err(|error| {
                GearError::config(format!(
                    "cannot encode the OpenCode V2 session request: {error}"
                ))
            })?;
            builder = builder
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(encoded);
        }
        let response = builder
            .send()
            .map_err(|error| GearError::config(format!("cannot reach {}: {error}", request.url)))?;
        let status = response.status().as_u16();
        let body = response.text().map_err(|error| {
            GearError::config(format!(
                "cannot read the response from {}: {error}",
                request.url
            ))
        })?;
        Ok(V2Response { status, body })
    }
}

/// The OpenCode 2 [`SessionClient`] backed by the local daemon.
pub struct V2SessionClient {
    base_url: String,
    directory: String,
    transport: Box<dyn V2Transport>,
}

impl V2SessionClient {
    /// Connect to a discovered service and scope sessions to `directory`.
    pub fn connect(
        registration: &ServiceRegistration,
        directory: impl Into<String>,
    ) -> Result<Self> {
        let transport = ReqwestV2Transport::new(registration)?;
        Ok(Self::with_transport(
            registration.url().to_string(),
            directory,
            Box::new(transport),
        ))
    }

    /// Build a client over an injected transport. This is the deterministic
    /// seam used by tests and by callers that supply their own HTTP stack.
    pub fn with_transport(
        base_url: impl Into<String>,
        directory: impl Into<String>,
        transport: Box<dyn V2Transport>,
    ) -> Self {
        Self {
            base_url: normalize_base_url(base_url.into()),
            directory: directory.into(),
            transport,
        }
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// Send one request, mapping transport failure, non-success status and
    /// non-JSON bodies to deterministic [`GearError`]s. A successful empty body
    /// (for example a `204`) yields `Ok(None)`.
    fn send(&self, method: &'static str, path: &str, body: Option<Value>) -> Result<Option<Value>> {
        let url = self.endpoint(path);
        let response = self
            .transport
            .send(V2Request {
                method,
                url: url.clone(),
                body,
            })
            .map_err(|error| {
                GearError::config(format!(
                    "OpenCode V2 session transport to {url} failed: {error}"
                ))
            })?;
        if !(200..300).contains(&response.status) {
            return Err(non_success(method, path, response.status, &response.body));
        }
        let text = response.body.trim();
        if text.is_empty() {
            return Ok(None);
        }
        let value = serde_json::from_str(text).map_err(|error| {
            GearError::config(format!(
                "OpenCode V2 session response from {path} is not valid JSON: {error}"
            ))
        })?;
        Ok(Some(value))
    }
}

impl SessionClient for V2SessionClient {
    /// Resolve the newest root session for this project directory, creating one
    /// only when the directory has no session yet.
    fn resolve_session(&mut self) -> Result<String> {
        let list_path = format!(
            "/api/session?directory={}&parentID=null&order=desc&limit=1",
            encode_component(&self.directory)
        );
        let listed = self
            .send("GET", &list_path, None)?
            .ok_or_else(|| malformed(&list_path, "empty response"))?;
        let data = listed
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed(&list_path, "missing the `data` array"))?;
        if let Some(id) = data
            .first()
            .and_then(|session| session.get("id"))
            .and_then(Value::as_str)
        {
            return Ok(id.to_string());
        }

        let created = self
            .send(
                "POST",
                "/api/session",
                Some(json!({"location": {"directory": self.directory}})),
            )?
            .ok_or_else(|| malformed("/api/session", "empty response"))?;
        created
            .get("data")
            .and_then(|data| data.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                GearError::config(
                    "OpenCode V2 session create response did not contain a session id",
                )
            })
    }

    fn select_agent(&mut self, session: &str, agent: &str) -> Result<()> {
        let path = format!("/api/session/{}/agent", encode_component(session));
        self.send("POST", &path, Some(json!({ "agent": agent })))?;
        Ok(())
    }

    fn select_model(
        &mut self,
        session: &str,
        provider_id: &str,
        model_id: &str,
        variant: Option<&str>,
    ) -> Result<()> {
        let mut model = Map::new();
        model.insert("id".to_string(), Value::String(model_id.to_string()));
        model.insert(
            "providerID".to_string(),
            Value::String(provider_id.to_string()),
        );
        // Omitted entirely when there is no resolved variant. Never `null`,
        // never an empty string, never a fabricated default.
        if let Some(variant) = variant {
            model.insert("variant".to_string(), Value::String(variant.to_string()));
        }
        let path = format!("/api/session/{}/model", encode_component(session));
        self.send(
            "POST",
            &path,
            Some(json!({ "model": Value::Object(model) })),
        )?;
        Ok(())
    }

    /// Read the session's effective Lead back. A response without an agent or
    /// model is an error: Gear refuses to rely on state it cannot verify.
    fn effective_lead(&self, session: &str) -> Result<EffectiveLead> {
        let path = format!("/api/session/{}", encode_component(session));
        let body = self
            .send("GET", &path, None)?
            .ok_or_else(|| malformed(&path, "empty response"))?;
        let data = body
            .get("data")
            .and_then(Value::as_object)
            .ok_or_else(|| malformed(&path, "missing the `data` object"))?;

        let agent = data
            .get("agent")
            .and_then(Value::as_str)
            .map(str::to_string);
        let model = data.get("model").and_then(Value::as_object);
        let provider_id = model
            .and_then(|model| model.get("providerID"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let model_id = model
            .and_then(|model| model.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let variant = model
            .and_then(|model| model.get("variant"))
            .and_then(Value::as_str)
            .map(str::to_string);

        if agent.is_none() || provider_id.is_none() || model_id.is_none() {
            return Err(GearError::config(format!(
                "OpenCode V2 session {session} did not report an effective agent/model"
            )));
        }
        Ok(EffectiveLead {
            agent,
            provider_id,
            model_id,
            variant,
        })
    }
}

fn malformed(path: &str, reason: &str) -> GearError {
    GearError::config(format!(
        "OpenCode V2 session response from {path} is malformed: {reason}"
    ))
}

/// A non-success status. Only the server's stable `_tag` discriminator (when
/// present and sanitized) is echoed; the response body is never rendered.
fn non_success(method: &str, path: &str, status: u16, body: &str) -> GearError {
    match safe_tag(body) {
        Some(tag) => GearError::config(format!(
            "OpenCode V2 session request {method} {path} failed with HTTP {status} ({tag})"
        )),
        None => GearError::config(format!(
            "OpenCode V2 session request {method} {path} failed with HTTP {status}"
        )),
    }
}

fn safe_tag(body: &str) -> Option<String> {
    let tag = serde_json::from_str::<Value>(body)
        .ok()?
        .get("_tag")?
        .as_str()?
        .to_string();
    let sanitized: String = tag
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
        .take(64)
        .collect();
    (!sanitized.is_empty()).then_some(sanitized)
}

/// Percent-encode one path segment or query value. Only RFC 3986 unreserved
/// characters survive, so project paths and session ids are always safe.
fn encode_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::compat::{select_session_lead, LeadSelection};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    const BASE: &str = "http://127.0.0.1:49374";
    const DIR: &str = "/work/app";
    const SESSION: &str = "ses_f4131e62affeMRROScDK23Dqv4";

    fn ok(body: Value) -> Result<V2Response> {
        Ok(V2Response {
            status: 200,
            body: body.to_string(),
        })
    }

    fn no_content() -> Result<V2Response> {
        Ok(V2Response {
            status: 204,
            body: String::new(),
        })
    }

    fn status(code: u16, body: &str) -> Result<V2Response> {
        Ok(V2Response {
            status: code,
            body: body.to_string(),
        })
    }

    #[derive(Clone, Default)]
    struct FakeTransport {
        state: Arc<FakeState>,
    }

    #[derive(Default)]
    struct FakeState {
        requests: Mutex<Vec<V2Request>>,
        responses: Mutex<VecDeque<Result<V2Response>>>,
    }

    impl FakeTransport {
        fn with(responses: Vec<Result<V2Response>>) -> Self {
            let transport = Self::default();
            transport
                .state
                .responses
                .lock()
                .expect("fake responses")
                .extend(responses);
            transport
        }

        fn requests(&self) -> Vec<V2Request> {
            self.state.requests.lock().expect("fake requests").clone()
        }
    }

    impl V2Transport for FakeTransport {
        fn send(&self, request: V2Request) -> Result<V2Response> {
            self.state
                .requests
                .lock()
                .expect("fake requests")
                .push(request);
            self.state
                .responses
                .lock()
                .expect("fake responses")
                .pop_front()
                .unwrap_or_else(|| Err(GearError::config("no fake response is queued")))
        }
    }

    fn client(transport: &FakeTransport) -> V2SessionClient {
        V2SessionClient::with_transport(BASE, DIR, Box::new(transport.clone()))
    }

    fn lead(variant: Option<&str>) -> LeadSelection {
        LeadSelection {
            level: "high".to_string(),
            agent: "lead-high".to_string(),
            provider_id: "openai".to_string(),
            model_id: "gpt-6-astra".to_string(),
            variant: variant.map(str::to_string),
        }
    }

    #[test]
    fn resolves_the_newest_existing_session_without_creating() {
        let fake = FakeTransport::with(vec![ok(json!({
            "data": [{"id": SESSION}],
            "cursor": {"previous": null, "next": null}
        }))]);
        let mut subject = client(&fake);

        assert_eq!(subject.resolve_session().unwrap(), SESSION);

        let requests = fake.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(
            requests[0].url,
            format!("{BASE}/api/session?directory=%2Fwork%2Fapp&parentID=null&order=desc&limit=1")
        );
        assert!(requests[0].body.is_none());
    }

    #[test]
    fn creates_a_session_when_the_project_has_none() {
        let fake = FakeTransport::with(vec![
            ok(json!({"data": [], "cursor": {}})),
            ok(json!({"data": {"id": SESSION}})),
        ]);
        let mut subject = client(&fake);

        assert_eq!(subject.resolve_session().unwrap(), SESSION);

        let requests = fake.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].method, "POST");
        assert_eq!(requests[1].url, format!("{BASE}/api/session"));
        assert_eq!(
            requests[1].body,
            Some(json!({"location": {"directory": DIR}}))
        );
    }

    #[test]
    fn switches_the_session_agent() {
        let fake = FakeTransport::with(vec![no_content()]);
        let mut subject = client(&fake);

        subject.select_agent(SESSION, "lead-high").unwrap();

        let requests = fake.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(
            requests[0].url,
            format!("{BASE}/api/session/{SESSION}/agent")
        );
        assert_eq!(requests[0].body, Some(json!({"agent": "lead-high"})));
    }

    #[test]
    fn switches_the_session_model_and_serializes_an_explicit_variant() {
        let fake = FakeTransport::with(vec![no_content()]);
        let mut subject = client(&fake);

        subject
            .select_model(SESSION, "openai", "gpt-6-astra", Some("low"))
            .unwrap();

        let requests = fake.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            format!("{BASE}/api/session/{SESSION}/model")
        );
        assert_eq!(
            requests[0].body,
            Some(json!({
                "model": {"id": "gpt-6-astra", "providerID": "openai", "variant": "low"}
            }))
        );
    }

    #[test]
    fn omits_the_variant_key_entirely_when_it_is_none() {
        let fake = FakeTransport::with(vec![no_content()]);
        let mut subject = client(&fake);

        subject
            .select_model(SESSION, "openai", "gpt-6-astra", None)
            .unwrap();

        let body = fake.requests()[0].body.clone().unwrap();
        assert_eq!(
            body,
            json!({"model": {"id": "gpt-6-astra", "providerID": "openai"}})
        );
        assert!(
            !body["model"]
                .as_object()
                .expect("model object")
                .contains_key("variant"),
            "a missing variant must not be serialized at all: {body}"
        );
    }

    #[test]
    fn parses_the_effective_session_lead() {
        let fake = FakeTransport::with(vec![ok(json!({
            "data": {
                "agent": "lead-high",
                "model": {"id": "gpt-6-astra", "providerID": "openai", "variant": "low"}
            }
        }))]);
        let subject = client(&fake);

        let effective = subject.effective_lead(SESSION).unwrap();
        assert_eq!(
            effective,
            EffectiveLead {
                agent: Some("lead-high".to_string()),
                provider_id: Some("openai".to_string()),
                model_id: Some("gpt-6-astra".to_string()),
                variant: Some("low".to_string()),
            }
        );
        assert_eq!(
            fake.requests()[0].url,
            format!("{BASE}/api/session/{SESSION}")
        );
    }

    #[test]
    fn parses_a_provider_default_effective_lead_without_a_variant() {
        let fake = FakeTransport::with(vec![ok(json!({
            "data": {
                "agent": "lead-high",
                "model": {"id": "gpt-6-astra", "providerID": "openai"}
            }
        }))]);
        let effective = client(&fake).effective_lead(SESSION).unwrap();
        assert_eq!(effective.variant, None);
        assert_eq!(effective.model_id.as_deref(), Some("gpt-6-astra"));
    }

    #[test]
    fn rejects_a_malformed_response() {
        let missing = FakeTransport::with(vec![ok(json!({"nope": true}))]);
        let error = client(&missing).effective_lead(SESSION).unwrap_err();
        assert!(error.to_string().contains("malformed"), "{error}");

        let not_json = FakeTransport::with(vec![status(200, "<html>nope</html>")]);
        let error = client(&not_json).effective_lead(SESSION).unwrap_err();
        assert!(error.to_string().contains("not valid JSON"), "{error}");
    }

    #[test]
    fn rejects_a_missing_effective_state() {
        let fake = FakeTransport::with(vec![ok(json!({"data": {"id": SESSION}}))]);
        let error = client(&fake).effective_lead(SESSION).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("did not report an effective agent/model"),
            "{error}"
        );
    }

    #[test]
    fn surfaces_a_non_success_status_without_echoing_the_body() {
        let fake = FakeTransport::with(vec![status(
            500,
            r#"{"_tag":"InternalError","message":"provider key sk-live-should-not-leak"}"#,
        )]);
        let mut subject = client(&fake);

        let error = subject.select_agent(SESSION, "lead-high").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("HTTP 500"), "{message}");
        assert!(message.contains("InternalError"), "{message}");
        assert!(
            !message.contains("sk-live-should-not-leak"),
            "the response body must never be echoed: {message}"
        );
    }

    #[test]
    fn surfaces_a_connection_failure() {
        let fake = FakeTransport::with(vec![Err(GearError::config("connection refused"))]);
        let mut subject = client(&fake);

        let error = subject.resolve_session().unwrap_err();
        let message = error.to_string();
        assert!(message.contains("connection refused"), "{message}");
        assert!(message.contains(BASE), "{message}");
    }

    #[test]
    fn renders_no_service_credential_anywhere() {
        let registration = ServiceRegistration::from_json(
            r#"{"url":"http://127.0.0.1:49374","password":"super-secret-token-value"}"#,
        )
        .unwrap();
        assert!(!format!("{registration:?}").contains("super-secret-token-value"));
        assert!(!format!("{}", registration.password()).contains("super-secret-token-value"));
        assert!(!format!("{:?}", registration.password()).contains("super-secret-token-value"));

        // A non-success body carrying credential-looking material is not echoed.
        let fake = FakeTransport::with(vec![status(500, "super-secret-token-value")]);
        let error = client(&fake).effective_lead(SESSION).unwrap_err();
        assert!(
            !error.to_string().contains("super-secret-token-value"),
            "{error}"
        );
    }

    #[test]
    fn reuses_the_policy_function_end_to_end_and_omits_a_missing_variant() {
        let fake = FakeTransport::with(vec![
            ok(json!({"data": [{"id": SESSION}], "cursor": {}})),
            no_content(),
            no_content(),
            ok(json!({
                "data": {
                    "agent": "lead-high",
                    "model": {"id": "gpt-6-astra", "providerID": "openai", "variant": "default"}
                }
            })),
        ]);
        let mut subject = client(&fake);

        let selection = select_session_lead(&mut subject, &lead(None)).unwrap();
        assert_eq!(selection.session_id, SESSION);
        assert_eq!(selection.lead.agent, "lead-high");

        let requests = fake.requests();
        assert_eq!(requests.len(), 4, "resolve, agent, model, effective");
        assert_eq!(requests[3].method, "GET");
        let model_body = requests[2].body.clone().unwrap();
        assert!(
            !model_body["model"]
                .as_object()
                .expect("model object")
                .contains_key("variant"),
            "the policy resolved no variant, so none may be sent: {model_body}"
        );
    }

    #[test]
    fn sends_the_resolved_variant_through_the_policy_function() {
        let fake = FakeTransport::with(vec![
            ok(json!({"data": [{"id": SESSION}], "cursor": {}})),
            no_content(),
            no_content(),
            ok(json!({
                "data": {
                    "agent": "lead-high",
                    "model": {"id": "gpt-6-astra", "providerID": "openai", "variant": "low"}
                }
            })),
        ]);
        let mut subject = client(&fake);

        let selection = select_session_lead(&mut subject, &lead(Some("low"))).unwrap();
        assert_eq!(selection.lead.variant.as_deref(), Some("low"));

        let model_body = fake.requests()[2].body.clone().unwrap();
        assert_eq!(model_body["model"]["variant"], json!("low"));
    }

    #[test]
    fn rejects_invalid_registration_documents() {
        assert!(ServiceRegistration::from_json("not json").is_err());
        assert!(ServiceRegistration::from_json("{}").is_err());
        assert!(ServiceRegistration::from_json(r#"{"url":"http://x"}"#).is_err());
        assert!(ServiceRegistration::from_json(r#"{"password":"p"}"#).is_err());
    }

    #[test]
    fn registration_path_follows_the_opencode_state_layout() {
        assert_eq!(
            service_registration_path(Some(Path::new("/state")), Path::new("/tmp/ocg-test-user")),
            PathBuf::from("/state/opencode/service.json")
        );
        assert_eq!(
            service_registration_path(None, Path::new("/tmp/ocg-test-user")),
            PathBuf::from("/tmp/ocg-test-user/.local/state/opencode/service.json")
        );
    }

    #[test]
    fn the_production_transport_builds_without_network_access() {
        let registration = ServiceRegistration::new("http://127.0.0.1:49374", "not-a-real-token");
        assert!(ReqwestV2Transport::new(&registration).is_ok());
    }

    #[test]
    fn component_encoding_is_strict() {
        assert_eq!(encode_component("/work/app a"), "%2Fwork%2Fapp%20a");
        assert_eq!(encode_component("ses_abc-123"), "ses_abc-123");
    }
}
