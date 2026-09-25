//! Loopback-only HTTP/1.1 + SSE control server.
//!
//! The server is a thin transport over [`ControlService`]. It is deliberately
//! not a general web framework:
//!
//! - It binds only a loopback address (`127.0.0.0/8` or `::1`); a wildcard or
//!   routable address is refused before the socket is created.
//! - It accepts a bounded number of concurrent clients and gives every socket a
//!   read and a write timeout, so a slow or stalled consumer is disconnected
//!   instead of pinning a connection.
//! - It caps the request head, the header count and the body, and rejects
//!   malformed, oversized and unsupported requests with a typed error envelope.
//! - It always closes the connection after a response, so there is no keep-alive
//!   state machine to get wrong.
//! - `GET /api/v1/events` first replays the durable journal from the requested
//!   cursor and then tails new events by polling the same authoritative reader.
//!   Event ids are `epoch:seq`; heartbeats are SSE comments and never carry an
//!   id, so they can never advance a client's resume position.
//!
//! No authentication, CORS, frontend or WebSocket support is provided. This is
//! local operator tooling, not a network service.

use crate::error::{GearError, Result};
use crate::orchestration::budget::{normalize_currency, Money};
use crate::orchestration::checkpoint::is_safe_id;
use crate::orchestration::control::{ControlError, ControlService, ReplaySlice};
use crate::orchestration::policy::ApprovalStatus;
use crate::orchestration::replay::{Cursor, EventEnvelope, SnapshotConfig};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Upper bound on the request head (request line + headers).
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Upper bound on the number of request headers.
pub const MAX_HEADERS: usize = 64;
/// Upper bound on a request body.
pub const MAX_BODY_BYTES: usize = 256 * 1024;
/// Upper bound on one serialized non-streaming response.
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Default bound on concurrently handled clients.
pub const DEFAULT_MAX_CLIENTS: usize = 16;
/// Default journal poll interval for an SSE tail.
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 200;
/// Default SSE heartbeat interval.
pub const DEFAULT_HEARTBEAT_MS: u64 = 10_000;
/// Default per-socket write timeout.
pub const DEFAULT_WRITE_TIMEOUT_MS: u64 = 15_000;
/// Default per-socket read timeout.
pub const DEFAULT_READ_TIMEOUT_MS: u64 = 10_000;

/// Server limits and timings. Every field has a bounded default.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Maximum number of clients handled at once. Further connections receive a
    /// `503` and are closed.
    pub max_clients: usize,
    /// Replay journal retention used by the service this server opens.
    pub snapshot: SnapshotConfig,
    /// How often an SSE tail polls the authoritative journal.
    pub poll_interval: Duration,
    /// How often an idle SSE tail emits a comment heartbeat.
    pub heartbeat: Duration,
    /// Per-socket write timeout; a slow consumer is dropped on expiry.
    pub write_timeout: Duration,
    /// Per-socket read timeout; a stalled request is dropped on expiry.
    pub read_timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_clients: DEFAULT_MAX_CLIENTS,
            snapshot: SnapshotConfig::default(),
            poll_interval: Duration::from_millis(DEFAULT_POLL_INTERVAL_MS),
            heartbeat: Duration::from_millis(DEFAULT_HEARTBEAT_MS),
            write_timeout: Duration::from_millis(DEFAULT_WRITE_TIMEOUT_MS),
            read_timeout: Duration::from_millis(DEFAULT_READ_TIMEOUT_MS),
        }
    }
}

impl ServerConfig {
    fn validate(&self) -> Result<()> {
        if self.max_clients == 0 || self.max_clients > 1024 {
            return Err(GearError::config(
                "control server max_clients must be between 1 and 1024",
            ));
        }
        if self.poll_interval < Duration::from_millis(10) {
            return Err(GearError::config(
                "control server poll_interval must be at least 10ms",
            ));
        }
        if self.heartbeat < self.poll_interval {
            return Err(GearError::config(
                "control server heartbeat must not be shorter than its poll interval",
            ));
        }
        if self.write_timeout.is_zero() || self.read_timeout.is_zero() {
            return Err(GearError::config(
                "control server socket timeouts must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// A bound control server. Call [`ControlServer::serve`] to run its accept loop.
pub struct ControlServer {
    listener: TcpListener,
    addr: SocketAddr,
    service: ControlService,
    config: ServerConfig,
    active: Arc<AtomicUsize>,
}

impl ControlServer {
    /// Validate the address and diagnostics, bind the listener, and open the
    /// authoritative service. Only loopback addresses are accepted.
    pub fn bind(addr: &str, root: &Path, config: ServerConfig) -> Result<Self> {
        let requested = parse_loopback_addr(addr)?;
        config.validate()?;
        let service = ControlService::open_with_config(root, config.snapshot)?;
        let listener = TcpListener::bind(requested).map_err(|error| {
            GearError::io(
                format!("cannot bind the control server to {requested}"),
                error,
            )
        })?;
        let bound = listener
            .local_addr()
            .map_err(|error| GearError::io("cannot read the control server address", error))?;
        // Defense in depth: even if the OS remapped the address, it must be
        // loopback.
        if !bound.ip().is_loopback() {
            return Err(GearError::config(format!(
                "control server bound a non-loopback address {bound}; refusing to serve"
            )));
        }
        Ok(Self {
            listener,
            addr: bound,
            service,
            config,
            active: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// The address the server is actually bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// A human-readable base URL for the bound loopback address.
    pub fn base_url(&self) -> String {
        match self.addr {
            SocketAddr::V4(_) => format!("http://{}", self.addr),
            SocketAddr::V6(_) => format!("http://[{}]:{}", self.addr.ip(), self.addr.port()),
        }
    }

    /// Run the accept loop until `stop` is set.
    pub fn serve(self, stop: Arc<AtomicBool>) -> Result<()> {
        self.listener.set_nonblocking(true).map_err(|error| {
            GearError::io(
                "cannot put the control server listener in nonblocking mode",
                error,
            )
        })?;
        loop {
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            match self.listener.accept() {
                Ok((stream, _peer)) => self.dispatch(stream, &stop),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    return Err(GearError::io("control server accept failed", error));
                }
            }
        }
    }

    fn dispatch(&self, mut stream: TcpStream, stop: &Arc<AtomicBool>) {
        let current = self.active.fetch_add(1, Ordering::SeqCst);
        if current >= self.config.max_clients {
            self.active.fetch_sub(1, Ordering::SeqCst);
            let _ = stream.set_write_timeout(Some(self.config.write_timeout));
            let _ = write_api_error(
                &mut stream,
                &ApiError::new(
                    503,
                    "overloaded",
                    "the control server is at its concurrent client limit",
                ),
            );
            return;
        }
        let guard = ClientGuard(Arc::clone(&self.active));
        let service = self.service.clone();
        let config = self.config.clone();
        let stop = Arc::clone(stop);
        let spawned = thread::Builder::new()
            .name("ocg-control-client".to_string())
            .spawn(move || {
                let _guard = guard;
                handle_client(stream, &service, &config, &stop);
            });
        if spawned.is_err() {
            // The closure (and its guard) is dropped, releasing the slot.
        }
    }
}

/// Decrements the active-client counter exactly once, even on panic.
struct ClientGuard(Arc<AtomicUsize>);

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Parse and validate a numeric loopback bind address.
pub fn parse_loopback_addr(raw: &str) -> Result<SocketAddr> {
    let trimmed = raw.trim();
    let socket: SocketAddr = trimmed.parse().map_err(|_| {
        GearError::config(format!(
            "control server address '{raw}' must be a numeric loopback IP and port, for example \
             127.0.0.1:0 or [::1]:8765"
        ))
    })?;
    if !socket.ip().is_loopback() {
        return Err(GearError::config(format!(
            "control server refuses to bind {socket}: only loopback addresses are allowed"
        )));
    }
    Ok(socket)
}

/// A typed transport error rendered as the shared JSON envelope.
#[derive(Debug)]
struct ApiError {
    status: u16,
    body: Value,
    allow: Option<&'static str>,
}

impl ApiError {
    fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            body: json!({ "error": { "code": code, "message": bounded(message.into()) } }),
            allow: None,
        }
    }

    fn method_not_allowed(allow: &'static str) -> Self {
        let mut error = Self::new(
            405,
            "method_not_allowed",
            format!("this route only allows {allow}"),
        );
        error.allow = Some(allow);
        error
    }

    fn from_control(error: ControlError) -> Self {
        Self {
            status: error.http_status(),
            body: error.to_json(),
            allow: None,
        }
    }

    fn to_json(&self) -> Value {
        self.body.clone()
    }
}

fn bounded(message: String) -> String {
    let redacted = crate::telemetry::task::redact(&message);
    const MAX: usize = 400;
    if redacted.len() <= MAX {
        return redacted;
    }
    let mut end = MAX;
    while end > 0 && !redacted.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &redacted[..end])
}

/// One parsed HTTP request. Only the fields this server uses are retained.
#[derive(Debug)]
struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    fn json_body(&self) -> std::result::Result<Value, ApiError> {
        if self.body.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&self.body)
            .map_err(|_| ApiError::new(400, "malformed_json", "the request body is not valid JSON"))
    }
}

fn handle_client(
    mut stream: TcpStream,
    service: &ControlService,
    config: &ServerConfig,
    stop: &Arc<AtomicBool>,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(config.read_timeout));
    let _ = stream.set_write_timeout(Some(config.write_timeout));
    let _ = stream.set_nodelay(true);

    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(error) => {
            let _ = write_api_error(&mut stream, &error);
            return;
        }
    };

    let route = match classify(&request) {
        Ok(route) => route,
        Err(error) => {
            let _ = write_api_error(&mut stream, &error);
            return;
        }
    };

    let outcome = match route {
        Route::Snapshot => respond_serialized(&mut stream, &service.snapshot()),
        Route::Approvals => {
            respond_serialized(&mut stream, &Ok::<_, ControlError>(service.approvals()))
        }
        Route::Resources => {
            respond_serialized(&mut stream, &Ok::<_, ControlError>(service.resources()))
        }
        Route::BudgetGet { mission } => respond_serialized(&mut stream, &service.budget(&mission)),
        Route::Approve { id } => resolve_approval(
            &mut stream,
            service,
            &request,
            &id,
            ApprovalStatus::Approved,
        ),
        Route::Reject { id } => resolve_approval(
            &mut stream,
            service,
            &request,
            &id,
            ApprovalStatus::Rejected,
        ),
        Route::BudgetPut { mission } => put_budget(&mut stream, service, &request, &mission),
        Route::Events { epoch, after } => {
            handle_events(&mut stream, service, config, stop, &request, epoch, after)
        }
    };
    let _ = outcome;
}

/// Handle approve/reject: mutate, then return the post-commit cursor.
fn resolve_approval(
    stream: &mut TcpStream,
    service: &ControlService,
    request: &Request,
    approval_id: &str,
    status: ApprovalStatus,
) -> std::io::Result<()> {
    let body = match request.json_body() {
        Ok(body) => body,
        Err(error) => return write_api_error(stream, &error),
    };
    let note = match body.get("note") {
        None | Some(Value::Null) => None,
        Some(Value::String(note)) => Some(note.clone()),
        Some(_) => {
            return write_api_error(
                stream,
                &ApiError::new(400, "invalid_request", "note must be a string"),
            )
        }
    };
    match service.resolve_approval(approval_id, status, note, now_unix()) {
        Ok((record, cursor)) => {
            let value = json!({ "cursor": cursor, "approval": record });
            write_json(stream, 200, &value)
        }
        Err(error) => write_api_error(stream, &ApiError::from_control(error)),
    }
}

/// Handle `PUT /api/v1/budgets/{mission}`: explicit hard-budget mutation.
fn put_budget(
    stream: &mut TcpStream,
    service: &ControlService,
    request: &Request,
    mission_id: &str,
) -> std::io::Result<()> {
    let body = match request.json_body() {
        Ok(body) => body,
        Err(error) => return write_api_error(stream, &error),
    };
    let Some(limit_micros) = body.get("limit_micros").and_then(Value::as_i64) else {
        return write_api_error(
            stream,
            &ApiError::new(
                400,
                "invalid_request",
                "limit_micros is required and must be an integer",
            ),
        );
    };
    let Some(currency) = body.get("currency").and_then(Value::as_str) else {
        return write_api_error(
            stream,
            &ApiError::new(
                400,
                "invalid_request",
                "currency is required and must be a string",
            ),
        );
    };
    let currency = match normalize_currency(currency) {
        Ok(currency) => currency,
        Err(error) => {
            return write_api_error(
                stream,
                &ApiError::new(400, "invalid_request", error.to_string()),
            )
        }
    };
    match service.set_budget(mission_id, Money::new(limit_micros, currency), now_unix()) {
        Ok(view) => write_json(
            stream,
            200,
            &serde_json::to_value(&view).unwrap_or(Value::Null),
        ),
        Err(error) => write_api_error(stream, &ApiError::from_control(error)),
    }
}

/// Handle `GET /api/v1/events`: replay, then live tail.
fn handle_events(
    stream: &mut TcpStream,
    service: &ControlService,
    config: &ServerConfig,
    stop: &Arc<AtomicBool>,
    request: &Request,
    epoch: u64,
    after: u64,
) -> std::io::Result<()> {
    // Last-Event-ID may only advance the same-epoch query cursor. A different
    // epoch is an explicit failure, never a silent reset.
    let mut cursor = after;
    if let Some(raw) = request.header("last-event-id") {
        let Some((last_epoch, last_seq)) = parse_sse_id(raw) else {
            return write_api_error(
                stream,
                &ApiError::new(
                    400,
                    "invalid_last_event_id",
                    "Last-Event-ID must be 'epoch:seq'",
                ),
            );
        };
        if last_epoch != epoch {
            return write_api_error(
                stream,
                &ApiError::from_control(ControlError::WrongEpoch {
                    expected: epoch,
                    got: last_epoch,
                }),
            );
        }
        cursor = cursor.max(last_seq);
    }

    // The initial replay is a hard boundary: a wrong/future/expired cursor is a
    // JSON error response, not a broken stream.
    let initial = match service.replay(epoch, cursor) {
        Ok(slice) => slice,
        Err(error) => return write_api_error(stream, &ApiError::from_control(error)),
    };

    write_sse_headers(stream)?;
    let mut last_seq = cursor;
    let mut last_write = Instant::now();
    if let ReplaySlice::Events(events) = initial {
        for envelope in &events {
            emit_event(stream, envelope)?;
            last_seq = envelope.cursor.seq;
        }
        last_write = Instant::now();
    }

    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        match service.replay(epoch, last_seq) {
            Ok(ReplaySlice::Events(events)) => {
                for envelope in &events {
                    emit_event(stream, envelope)?;
                    last_seq = envelope.cursor.seq;
                }
                last_write = Instant::now();
            }
            Ok(ReplaySlice::Empty) => {}
            Err(error) => {
                // A live failure (retention expired, epoch changed, authority
                // lost) is explicit and closes the stream.
                emit_sse_reset(stream, service, &error)?;
                return Ok(());
            }
        }
        if last_write.elapsed() >= config.heartbeat {
            emit_heartbeat(stream)?;
            last_write = Instant::now();
        }
        thread::sleep(config.poll_interval);
    }
}

fn emit_event(stream: &mut TcpStream, envelope: &EventEnvelope) -> std::io::Result<()> {
    let data = match serde_json::to_string(envelope) {
        Ok(data) => data,
        Err(_) => {
            let frame = "event: reset_required\ndata: {\"error\":{\"code\":\"serialization_error\",\"message\":\"the journal event could not be serialized\"}}\n\n";
            stream.write_all(frame.as_bytes())?;
            stream.flush()?;
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "journal event serialization failed",
            ));
        }
    };
    let frame = format!(
        "id: {}:{}\nevent: {}\ndata: {}\n\n",
        envelope.cursor.epoch,
        envelope.cursor.seq,
        envelope.event.kind(),
        data
    );
    stream.write_all(frame.as_bytes())?;
    stream.flush()
}

fn emit_heartbeat(stream: &mut TcpStream) -> std::io::Result<()> {
    // A comment carries no `id:` and therefore cannot advance Last-Event-ID.
    let frame = format!(": heartbeat {}\n\n", now_unix());
    stream.write_all(frame.as_bytes())?;
    stream.flush()
}

fn emit_sse_reset(
    stream: &mut TcpStream,
    service: &ControlService,
    error: &ControlError,
) -> std::io::Result<()> {
    let mut data = error.to_json();
    if let Ok(cursor) = service.head() {
        data["current_cursor"] = json!(cursor);
    }
    let frame = format!("event: reset_required\ndata: {}\n\n", data);
    stream.write_all(frame.as_bytes())?;
    stream.flush()
}

fn write_sse_headers(stream: &mut TcpStream) -> std::io::Result<()> {
    let head = "HTTP/1.1 200 OK\r\n\
                Content-Type: text/event-stream\r\n\
                Cache-Control: no-cache, no-store\r\n\
                X-Accel-Buffering: no\r\n\
                Connection: close\r\n\
                \r\n";
    stream.write_all(head.as_bytes())?;
    stream.flush()
}

fn respond_serialized<T: Serialize>(
    stream: &mut TcpStream,
    outcome: &std::result::Result<T, ControlError>,
) -> std::io::Result<()> {
    match outcome {
        Ok(value) => match serde_json::to_vec(value) {
            Ok(body) if body.len() <= MAX_RESPONSE_BYTES => {
                write_response(stream, 200, "application/json", &body, &[])
            }
            Ok(_) => write_api_error(
                stream,
                &ApiError::new(
                    503,
                    "response_too_large",
                    "the serialized response exceeds the control API limit",
                ),
            ),
            Err(_) => write_api_error(
                stream,
                &ApiError::new(
                    500,
                    "serialization_error",
                    "the response could not be serialized",
                ),
            ),
        },
        Err(error) => write_api_error(stream, &ApiError::from_control(error.clone())),
    }
}

fn write_json(stream: &mut TcpStream, status: u16, value: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    write_response(stream, status, "application/json", &body, &[])
}

fn write_api_error(stream: &mut TcpStream, error: &ApiError) -> std::io::Result<()> {
    let body = serde_json::to_vec(&error.to_json()).unwrap_or_else(|_| b"{}".to_vec());
    let extra: Vec<(&str, String)> = error
        .allow
        .map(|allow| vec![("Allow", allow.to_string())])
        .unwrap_or_default();
    write_response(stream, error.status, "application/json", &body, &extra)
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    extra: &[(&str, String)],
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n",
        reason(status),
        body.len()
    );
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn read_request(stream: &mut TcpStream) -> std::result::Result<Request, ApiError> {
    let mut buffer: Vec<u8> = Vec::with_capacity(1024);
    let head_end = loop {
        if let Some(index) = find_head_end(&buffer) {
            if index > MAX_HEADER_BYTES {
                return Err(ApiError::new(
                    431,
                    "headers_too_large",
                    format!("the request head exceeds {MAX_HEADER_BYTES} bytes"),
                ));
            }
            break index;
        }
        if buffer.len() > MAX_HEADER_BYTES {
            return Err(ApiError::new(
                431,
                "headers_too_large",
                format!("the request head exceeds {MAX_HEADER_BYTES} bytes"),
            ));
        }
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).map_err(|_| {
            ApiError::new(400, "malformed_request", "the request could not be read")
        })?;
        if read == 0 {
            return Err(ApiError::new(
                400,
                "malformed_request",
                "the connection closed before the request head was complete",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_HEADER_BYTES + MAX_BODY_BYTES {
            return Err(ApiError::new(
                413,
                "payload_too_large",
                "the request is larger than the accepted maximum",
            ));
        }
    };

    let head = std::str::from_utf8(&buffer[..head_end]).map_err(|_| {
        ApiError::new(
            400,
            "malformed_request",
            "the request head is not valid UTF-8",
        )
    })?;
    let mut lines = head.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| ApiError::new(400, "malformed_request", "the request line is missing"))?;
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default().to_ascii_uppercase();
    let target = parts.next().unwrap_or_default().to_string();
    let version = parts.next().unwrap_or_default();
    if parts.next().is_some() || target.is_empty() || version.is_empty() {
        return Err(ApiError::new(
            400,
            "malformed_request",
            "the request line is malformed",
        ));
    }
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(ApiError::new(
            505,
            "http_version_not_supported",
            "only HTTP/1.0 and HTTP/1.1 are supported",
        ));
    }
    if !matches!(method.as_str(), "GET" | "POST" | "PUT") {
        return Err(ApiError::method_not_allowed("GET, POST, PUT"));
    }

    let mut headers = HashMap::new();
    let mut count = 0usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            return Err(ApiError::new(
                400,
                "malformed_request",
                "folded request headers are not accepted",
            ));
        }
        count += 1;
        if count > MAX_HEADERS {
            return Err(ApiError::new(
                431,
                "headers_too_large",
                format!("the request has more than {MAX_HEADERS} headers"),
            ));
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(ApiError::new(
                400,
                "malformed_request",
                "a request header is missing its colon",
            ));
        };
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        {
            return Err(ApiError::new(
                400,
                "malformed_request",
                "a request header has an invalid name",
            ));
        }
        if headers.insert(name, value.trim().to_string()).is_some() {
            return Err(ApiError::new(
                400,
                "malformed_request",
                "duplicate request headers are not accepted",
            ));
        }
    }

    if headers.contains_key("transfer-encoding") {
        return Err(ApiError::new(
            400,
            "unsupported_transfer_encoding",
            "chunked request bodies are not supported",
        ));
    }

    let content_length = match headers.get("content-length") {
        Some(value) => value.trim().parse::<usize>().map_err(|_| {
            ApiError::new(
                400,
                "malformed_request",
                "Content-Length must be a non-negative integer",
            )
        })?,
        None => 0,
    };
    if content_length > MAX_BODY_BYTES {
        return Err(ApiError::new(
            413,
            "payload_too_large",
            format!("the request body exceeds the {MAX_BODY_BYTES} byte limit"),
        ));
    }

    let body_start = head_end + 4;
    let mut body = buffer[body_start..].to_vec();
    if body.len() > content_length {
        return Err(ApiError::new(
            400,
            "malformed_request",
            "bytes after the declared request body are not accepted",
        ));
    }
    while body.len() < content_length {
        let remaining = content_length - body.len();
        let mut chunk = [0u8; 4096];
        let take = remaining.min(chunk.len());
        let read = stream.read(&mut chunk[..take]).map_err(|_| {
            ApiError::new(
                400,
                "malformed_request",
                "the request body could not be read",
            )
        })?;
        if read == 0 {
            return Err(ApiError::new(
                400,
                "malformed_request",
                "the request body ended before Content-Length bytes arrived",
            ));
        }
        body.extend_from_slice(&chunk[..read]);
    }

    let (path, query) = parse_target(&target)?;
    Ok(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_target(target: &str) -> std::result::Result<(String, HashMap<String, String>), ApiError> {
    let (raw_path, raw_query) = match target.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (target, None),
    };
    if !raw_path.starts_with('/') {
        return Err(ApiError::new(
            400,
            "malformed_request",
            "the request target must be an absolute path",
        ));
    }
    let path = percent_decode(raw_path);
    let mut query = HashMap::new();
    if let Some(raw_query) = raw_query {
        for pair in raw_query.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = match pair.split_once('=') {
                Some((key, value)) => (percent_decode(key), percent_decode(value)),
                None => (percent_decode(pair), String::new()),
            };
            query.insert(key, value);
        }
    }
    Ok((path, query))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                out.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The classified route for a request.
enum Route {
    Snapshot,
    Events { epoch: u64, after: u64 },
    Approvals,
    Approve { id: String },
    Reject { id: String },
    Resources,
    BudgetGet { mission: String },
    BudgetPut { mission: String },
}

fn classify(request: &Request) -> std::result::Result<Route, ApiError> {
    let segments: Vec<&str> = request
        .path
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();

    let allowed = allowed_methods(&segments);
    if let Some(methods) = allowed {
        let is_route = matches!(
            (&request.method[..], segments.as_slice()),
            ("GET", ["api", "v1", "snapshot"])
                | ("GET", ["api", "v1", "events"])
                | ("GET", ["api", "v1", "approvals"])
                | ("GET", ["api", "v1", "resources"])
                | ("GET", ["api", "v1", "budgets", _])
                | ("PUT", ["api", "v1", "budgets", _])
                | ("POST", ["api", "v1", "approvals", _, "approve"])
                | ("POST", ["api", "v1", "approvals", _, "reject"])
        );
        if !is_route {
            return Err(ApiError::method_not_allowed(methods));
        }
    }

    match (request.method.as_str(), segments.as_slice()) {
        ("GET", ["api", "v1", "snapshot"]) => Ok(Route::Snapshot),
        ("GET", ["api", "v1", "events"]) => {
            let after = request
                .query
                .get("after")
                .ok_or_else(|| ApiError::new(400, "invalid_request", "'after' is required"))
                .and_then(|raw| {
                    parse_u64(raw).ok_or_else(|| {
                        ApiError::new(
                            400,
                            "invalid_request",
                            "'after' must be a non-negative integer",
                        )
                    })
                })?;
            let epoch = request
                .query
                .get("epoch")
                .ok_or_else(|| ApiError::new(400, "invalid_request", "'epoch' is required"))
                .and_then(|raw| {
                    parse_u64(raw).filter(|epoch| *epoch > 0).ok_or_else(|| {
                        ApiError::new(400, "invalid_request", "'epoch' must be a positive integer")
                    })
                })?;
            Ok(Route::Events { epoch, after })
        }
        ("GET", ["api", "v1", "approvals"]) => Ok(Route::Approvals),
        ("POST", ["api", "v1", "approvals", id, "approve"]) => {
            Ok(Route::Approve { id: safe_id(id)? })
        }
        ("POST", ["api", "v1", "approvals", id, "reject"]) => {
            Ok(Route::Reject { id: safe_id(id)? })
        }
        ("GET", ["api", "v1", "resources"]) => Ok(Route::Resources),
        ("GET", ["api", "v1", "budgets", mission]) => Ok(Route::BudgetGet {
            mission: safe_id(mission)?,
        }),
        ("PUT", ["api", "v1", "budgets", mission]) => Ok(Route::BudgetPut {
            mission: safe_id(mission)?,
        }),
        _ => Err(ApiError::new(404, "not_found", "no such control route")),
    }
}

fn allowed_methods(segments: &[&str]) -> Option<&'static str> {
    match segments {
        ["api", "v1", "snapshot"]
        | ["api", "v1", "events"]
        | ["api", "v1", "approvals"]
        | ["api", "v1", "resources"] => Some("GET"),
        ["api", "v1", "budgets", _] => Some("GET, PUT"),
        ["api", "v1", "approvals", _, "approve"] | ["api", "v1", "approvals", _, "reject"] => {
            Some("POST")
        }
        _ => None,
    }
}

fn safe_id(value: &str) -> std::result::Result<String, ApiError> {
    if !is_safe_id(value) {
        return Err(ApiError::new(
            400,
            "invalid_id",
            format!("'{value}' is not a safe identifier"),
        ));
    }
    Ok(value.to_string())
}

fn parse_u64(raw: &str) -> Option<u64> {
    raw.trim().parse::<u64>().ok()
}

fn parse_sse_id(raw: &str) -> Option<(u64, u64)> {
    let (epoch, seq) = raw.trim().split_once(':')?;
    Some((epoch.trim().parse().ok()?, seq.trim().parse().ok()?))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        _ => "Error",
    }
}

/// Parse the `epoch:seq` cursor carried by an SSE `Last-Event-ID` header.
pub fn parse_last_event_id(raw: &str) -> Option<Cursor> {
    parse_sse_id(raw).map(|(epoch, seq)| Cursor { epoch, seq })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_addresses_are_accepted_and_wildcards_refused() {
        assert!(parse_loopback_addr("127.0.0.1:0").is_ok());
        assert!(parse_loopback_addr("[::1]:8765").is_ok());
        assert!(parse_loopback_addr("127.5.5.5:1").is_ok());
        for refused in [
            "0.0.0.0:0",
            "192.168.1.1:80",
            "[::]:80",
            "example.test:80",
            "nope",
            "",
        ] {
            assert!(parse_loopback_addr(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn sse_ids_round_trip() {
        assert_eq!(parse_sse_id("1:42"), Some((1, 42)));
        assert_eq!(
            parse_last_event_id("7:9"),
            Some(Cursor { epoch: 7, seq: 9 })
        );
        assert_eq!(parse_sse_id("1"), None);
        assert_eq!(parse_sse_id("a:b"), None);
        assert_eq!(parse_sse_id("1:-1"), None);
    }

    #[test]
    fn percent_decoding_is_lossy_but_never_panics() {
        assert_eq!(percent_decode("apr%2D1"), "apr-1");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn target_parsing_splits_path_and_query() {
        let (path, query) = parse_target("/api/v1/events?epoch=1&after=0").unwrap();
        assert_eq!(path, "/api/v1/events");
        assert_eq!(query.get("epoch").map(String::as_str), Some("1"));
        assert_eq!(query.get("after").map(String::as_str), Some("0"));
        assert!(parse_target("api/v1").is_err());
    }
}
