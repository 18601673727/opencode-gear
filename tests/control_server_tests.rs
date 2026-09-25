//! Integration tests for the Phase 2B-2 loopback control server.
//!
//! Every test uses a real TCP listener on an ephemeral loopback port and a raw
//! HTTP client, so the request parsing, routing, error envelopes, SSE framing
//! and timeouts are exercised end to end. No external network is touched.

use opencode_gear::control_server::{ControlServer, ServerConfig, MAX_BODY_BYTES};
use opencode_gear::orchestration::{
    mission, policy, ApprovalRequest, ApprovalStatus, Cursor, DomainEvent, Mission, PolicyAction,
    SnapshotConfig, SnapshotService,
};
use opencode_gear::resources::{self, ResourceIdentity, ResourceRegistry};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn mission(index: u64, now: i64) -> Mission {
    Mission::admit(&format!("task-srv-{index:016}"), "task", "session-1", now)
}

fn append_mission(service: &SnapshotService, index: u64) -> Cursor {
    service
        .append(DomainEvent::MissionUpsert {
            mission: mission(index, index as i64),
        })
        .unwrap()
        .expect("a distinct mission is a state change")
}

fn pending_approval(root: &Path, mission: &Mission, now: i64) -> String {
    let approval_id = policy::approval_id(
        &mission.mission_id,
        mission.generation,
        PolicyAction::EnsureExecution,
        None,
    );
    let request = ApprovalRequest {
        approval_id: approval_id.clone(),
        mission_id: mission.mission_id.clone(),
        generation: mission.generation,
        action: PolicyAction::EnsureExecution,
        current_execution_id: None,
        requested_at: now,
    };
    policy::ensure_pending(root, &request).unwrap();
    approval_id
}

// -- raw HTTP client ---------------------------------------------------------

#[derive(Debug)]
struct RawResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl RawResponse {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|error| {
            panic!("response body is not JSON: {error}\nbody={}", self.body)
        })
    }
}

fn send_and_read(addr: SocketAddr, request: &str) -> RawResponse {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))
        .expect("connect to the control server");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    parse_response(&bytes)
}

fn parse_response(bytes: &[u8]) -> RawResponse {
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response has a header terminator");
    let head = String::from_utf8_lossy(&bytes[..split]);
    let body = String::from_utf8_lossy(&bytes[split + 4..]).into_owned();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|line| {
            line.split_once(':')
                .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();
    RawResponse {
        status,
        headers,
        body,
    }
}

fn raw_request(
    addr: SocketAddr,
    method: &str,
    target: &str,
    body: Option<&str>,
    extra: &[(&str, &str)],
) -> RawResponse {
    let mut request =
        format!("{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    for (name, value) in extra {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    if let Some(body) = body {
        request.push_str(body);
    }
    send_and_read(addr, &request)
}

// -- SSE client --------------------------------------------------------------

#[derive(Debug, Default, Clone)]
struct SseFrame {
    id: Option<String>,
    event: Option<String>,
    data: Option<String>,
}

struct SseReader {
    stream: TcpStream,
    buffer: Vec<u8>,
    deadline: Instant,
}

fn open_events(addr: SocketAddr, target: &str, last_event_id: Option<&str>) -> SseReader {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))
        .expect("connect to the event stream");
    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .unwrap();
    let mut request =
        format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n");
    if let Some(value) = last_event_id {
        request.push_str(&format!("Last-Event-ID: {value}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).unwrap();

    let mut buffer = Vec::new();
    loop {
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buffer[..index]).into_owned();
            let status = head
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse::<u16>().ok())
                .unwrap_or(0);
            assert_eq!(status, 200, "expected an SSE 200, got:\n{head}");
            buffer.drain(..index + 4);
            break;
        }
        let mut chunk = [0u8; 1024];
        let read = stream.read(&mut chunk).expect("read SSE handshake");
        if read == 0 {
            panic!("the stream closed before the SSE handshake completed");
        }
        buffer.extend_from_slice(&chunk[..read]);
    }

    SseReader {
        stream,
        buffer,
        deadline: Instant::now() + Duration::from_secs(10),
    }
}

impl SseReader {
    fn read_frame(&mut self) -> Option<SseFrame> {
        loop {
            if let Some(index) = self.buffer.windows(2).position(|window| window == b"\n\n") {
                let frame: Vec<u8> = self.buffer.drain(..index + 2).collect();
                return Some(parse_sse_frame(&frame));
            }
            if Instant::now() >= self.deadline {
                return None;
            }
            let mut chunk = [0u8; 1024];
            match self.stream.read(&mut chunk) {
                Ok(0) => return None,
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue
                }
                Err(_) => return None,
            }
        }
    }
}

fn parse_sse_frame(frame: &[u8]) -> SseFrame {
    let text = String::from_utf8_lossy(frame);
    let mut parsed = SseFrame::default();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("id: ") {
            parsed.id = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("event: ") {
            parsed.event = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("data: ") {
            parsed.data = Some(value.to_string());
        }
    }
    parsed
}

// -- server harness ----------------------------------------------------------

struct TestServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    fn start(root: &Path, config: ServerConfig) -> Self {
        let server = ControlServer::bind("127.0.0.1:0", root, config).expect("bind the server");
        let addr = server.local_addr();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let _ = server.serve(thread_stop);
        });
        Self {
            addr,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn project() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// -- snapshot ----------------------------------------------------------------

#[test]
fn snapshot_route_returns_the_authority_and_cursor_atomically() {
    let dir = project();
    let service = SnapshotService::open(dir.path()).unwrap();
    let first = mission(1, 1);
    service
        .append(DomainEvent::MissionUpsert {
            mission: first.clone(),
        })
        .unwrap();
    let server = TestServer::start(dir.path(), ServerConfig::default());

    let response = raw_request(server.addr, "GET", "/api/v1/snapshot", None, &[]);
    assert_eq!(response.status, 200);
    let value = response.json();
    assert_eq!(value["cursor"]["epoch"], 1);
    assert_eq!(value["cursor"]["seq"], 1);
    assert!(value["snapshot"]["missions"]
        .get(&first.mission_id)
        .is_some());
}

#[test]
fn mutation_after_http_snapshot_is_replayed_without_a_gap() {
    let dir = project();
    let service = SnapshotService::open(dir.path()).unwrap();
    append_mission(&service, 1);
    let server = TestServer::start(dir.path(), ServerConfig::default());

    let response = raw_request(server.addr, "GET", "/api/v1/snapshot", None, &[]);
    assert_eq!(response.status, 200);
    let snapshot = response.json();
    assert_eq!(snapshot["api_version"], "v1");
    assert_eq!(snapshot["schema_version"], 1);
    let epoch = snapshot["cursor"]["epoch"].as_u64().unwrap();
    let seq = snapshot["cursor"]["seq"].as_u64().unwrap();

    let committed = append_mission(&service, 2);
    let mut reader = open_events(
        server.addr,
        &format!("/api/v1/events?epoch={epoch}&after={seq}"),
        None,
    );
    let frame = reader.read_frame().expect("post-snapshot event");
    assert_eq!(
        frame.id,
        Some(format!("{}:{}", committed.epoch, committed.seq))
    );
    assert_eq!(frame.event.as_deref(), Some("mission_upsert"));
}

// -- approvals ---------------------------------------------------------------

#[test]
fn approval_mutations_commit_through_the_authority_and_return_the_cursor() {
    let dir = project();
    let service = SnapshotService::open(dir.path()).unwrap();
    let admitted = mission(1, 1);
    mission::save(dir.path(), &admitted).unwrap();
    let approval_id = pending_approval(dir.path(), &admitted, 1);
    let server = TestServer::start(dir.path(), ServerConfig::default());

    let before = service.head().unwrap();
    let approved = raw_request(
        server.addr,
        "POST",
        &format!("/api/v1/approvals/{approval_id}/approve"),
        Some("{\"note\":\"looks good\"}"),
        &[],
    );
    assert_eq!(approved.status, 200, "body={}", approved.body);
    let value = approved.json();
    assert_eq!(value["approval"]["status"], "approved");
    let cursor = value["cursor"]["seq"].as_u64().unwrap();
    assert!(
        cursor > before.seq,
        "the mutation returns a post-commit cursor"
    );
    assert_eq!(
        policy::load_approval(dir.path(), &approval_id)
            .unwrap()
            .unwrap()
            .status,
        ApprovalStatus::Approved
    );
    let snapshot = service.snapshot().unwrap();
    assert_eq!(
        snapshot.approvals[&approval_id].status,
        ApprovalStatus::Approved
    );
    let replay = service.replay_after(before);
    let events = match replay {
        opencode_gear::orchestration::ReplayAfter::Success { events } => events,
        other => panic!("expected approval event, got {other:?}"),
    };
    assert!(events.iter().any(|event| {
        matches!(
            &event.event,
            DomainEvent::ApprovalUpsert { approval }
                if approval.approval_id == approval_id
                    && approval.status == ApprovalStatus::Approved
        )
    }));

    let rejected = raw_request(
        server.addr,
        "POST",
        &format!("/api/v1/approvals/{approval_id}/reject"),
        None,
        &[],
    );
    assert_eq!(rejected.status, 200);
    assert_eq!(rejected.json()["approval"]["status"], "rejected");
    assert_eq!(
        policy::load_approval(dir.path(), &approval_id)
            .unwrap()
            .unwrap()
            .status,
        ApprovalStatus::Rejected
    );
}

#[test]
fn unknown_approval_is_a_typed_404() {
    let dir = project();
    SnapshotService::open(dir.path()).unwrap();
    let server = TestServer::start(dir.path(), ServerConfig::default());
    let response = raw_request(
        server.addr,
        "POST",
        "/api/v1/approvals/apr-missing/approve",
        None,
        &[],
    );
    assert_eq!(response.status, 404);
    assert_eq!(response.json()["error"]["code"], "not_found");
}

#[test]
fn approvals_listing_reports_durable_records() {
    let dir = project();
    SnapshotService::open(dir.path()).unwrap();
    let admitted = mission(1, 1);
    mission::save(dir.path(), &admitted).unwrap();
    let approval_id = pending_approval(dir.path(), &admitted, 1);
    let server = TestServer::start(dir.path(), ServerConfig::default());

    let response = raw_request(server.addr, "GET", "/api/v1/approvals", None, &[]);
    assert_eq!(response.status, 200);
    let value = response.json();
    let approvals = value["approvals"].as_array().unwrap();
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0]["approval_id"], approval_id);
}

// -- budget ------------------------------------------------------------------

#[test]
fn budget_put_and_get_round_trip_authoritatively() {
    let dir = project();
    let service = SnapshotService::open(dir.path()).unwrap();
    let admitted = mission(1, 1);
    let mission_id = admitted.mission_id.clone();
    mission::save(dir.path(), &admitted).unwrap();
    let server = TestServer::start(dir.path(), ServerConfig::default());

    let before = service.head().unwrap();
    let put = raw_request(
        server.addr,
        "PUT",
        &format!("/api/v1/budgets/{mission_id}"),
        Some("{\"limit_micros\":500000,\"currency\":\"usd\"}"),
        &[],
    );
    assert_eq!(put.status, 200, "body={}", put.body);
    let value = put.json();
    assert_eq!(value["budget"]["hard_limit_micros"], 500000);
    assert_eq!(value["budget"]["currency"], "USD");
    assert!(value["cursor"]["seq"].as_u64().unwrap() > before.seq);

    let get = raw_request(
        server.addr,
        "GET",
        &format!("/api/v1/budgets/{mission_id}"),
        None,
        &[],
    );
    assert_eq!(get.status, 200);
    assert_eq!(get.json()["budget"]["hard_limit_micros"], 500000);

    let missing = raw_request(
        server.addr,
        "GET",
        "/api/v1/budgets/task-missing",
        None,
        &[],
    );
    assert_eq!(missing.status, 404);

    let invalid = raw_request(
        server.addr,
        "PUT",
        &format!("/api/v1/budgets/{mission_id}"),
        Some("{\"limit_micros\":0,\"currency\":\"USD\"}"),
        &[],
    );
    assert_eq!(invalid.status, 400);
    assert_eq!(invalid.json()["error"]["code"], "invalid_request");
}

// -- events ------------------------------------------------------------------

#[test]
fn events_replay_then_live_tail_is_ordered_and_gap_free() {
    let dir = project();
    let service = SnapshotService::open(dir.path()).unwrap();
    append_mission(&service, 1);
    append_mission(&service, 2);
    let server = TestServer::start(dir.path(), ServerConfig::default());

    let mut reader = open_events(server.addr, "/api/v1/events?epoch=1&after=0", None);
    let mut ids = Vec::new();
    for _ in 0..2 {
        let frame = reader.read_frame().expect("replayed frame");
        assert_eq!(frame.event.as_deref(), Some("mission_upsert"));
        ids.push(frame.id.expect("replayed frame has an id"));
    }

    // Append the remaining events from another thread while the stream is open.
    // The tail must pick them up in order with no gap behind the replay.
    let root = dir.path().to_path_buf();
    let writer = thread::spawn(move || {
        let service = SnapshotService::open(&root).unwrap();
        for index in 3..=10 {
            append_mission(&service, index);
        }
    });
    for _ in 2..10 {
        let frame = reader.read_frame().expect("live frame");
        ids.push(frame.id.expect("live frame has an id"));
    }
    writer.join().unwrap();

    let expected: Vec<String> = (1..=10).map(|seq| format!("1:{seq}")).collect();
    assert_eq!(ids, expected);
}

#[test]
fn last_event_id_resumes_and_only_advances_the_same_epoch_cursor() {
    let dir = project();
    let service = SnapshotService::open(dir.path()).unwrap();
    for index in 1..=5 {
        append_mission(&service, index);
    }
    let server = TestServer::start(dir.path(), ServerConfig::default());

    // Reconnect after id 1:2: the stream resumes at 1:3, not from the start.
    let mut resumed = open_events(server.addr, "/api/v1/events?epoch=1&after=0", Some("1:2"));
    assert_eq!(resumed.read_frame().unwrap().id.as_deref(), Some("1:3"));

    // Last-Event-ID may only advance the query cursor: after=4 wins over 1:2.
    let mut advanced = open_events(server.addr, "/api/v1/events?epoch=1&after=4", Some("1:2"));
    assert_eq!(advanced.read_frame().unwrap().id.as_deref(), Some("1:5"));

    // After=0 with a later Last-Event-ID advances to just past it.
    let mut later = open_events(server.addr, "/api/v1/events?epoch=1&after=0", Some("1:4"));
    assert_eq!(later.read_frame().unwrap().id.as_deref(), Some("1:5"));

    // A different epoch is an explicit failure, never a silent reset.
    let conflict = raw_request(
        server.addr,
        "GET",
        "/api/v1/events?epoch=1&after=0",
        None,
        &[("Last-Event-ID", "2:0")],
    );
    assert_eq!(conflict.status, 409);
    assert_eq!(conflict.json()["error"]["code"], "wrong_epoch");
}

#[test]
fn invalid_cursors_are_explicit() {
    let dir = project();
    let service = SnapshotService::open(dir.path()).unwrap();
    append_mission(&service, 1);
    let server = TestServer::start(dir.path(), ServerConfig::default());

    let wrong_epoch = raw_request(
        server.addr,
        "GET",
        "/api/v1/events?epoch=9&after=0",
        None,
        &[],
    );
    assert_eq!(wrong_epoch.status, 409);
    assert_eq!(wrong_epoch.json()["error"]["code"], "wrong_epoch");

    let future = raw_request(
        server.addr,
        "GET",
        "/api/v1/events?epoch=1&after=99",
        None,
        &[],
    );
    assert_eq!(future.status, 409);
    assert_eq!(future.json()["error"]["code"], "future_cursor");

    let missing = raw_request(server.addr, "GET", "/api/v1/events", None, &[]);
    assert_eq!(missing.status, 400);
    assert_eq!(missing.json()["error"]["code"], "invalid_request");

    let malformed = raw_request(
        server.addr,
        "GET",
        "/api/v1/events?epoch=abc&after=0",
        None,
        &[],
    );
    assert_eq!(malformed.status, 400);
    assert_eq!(malformed.json()["error"]["code"], "invalid_request");
}

#[test]
fn a_pruned_prefix_is_expired_and_never_partially_replayed() {
    let dir = project();
    let config = ServerConfig {
        snapshot: SnapshotConfig::new(2),
        ..ServerConfig::default()
    };
    let server = TestServer::start(dir.path(), config);

    // Pruning is a property of the writer's retention, so append through a
    // service configured with the same bound.
    let service = SnapshotService::open_with_config(dir.path(), SnapshotConfig::new(2)).unwrap();
    for index in 1..=4 {
        append_mission(&service, index);
    }

    let expired = raw_request(
        server.addr,
        "GET",
        "/api/v1/events?epoch=1&after=0",
        None,
        &[],
    );
    assert_eq!(expired.status, 410);
    let value = expired.json();
    assert_eq!(value["error"]["code"], "replay_expired");
    assert_eq!(value["error"]["requested_seq"], 0);
    assert!(value["error"]["floor_seq"].as_u64().unwrap() >= 1);
}

#[test]
fn heartbeats_carry_no_event_id() {
    let dir = project();
    SnapshotService::open(dir.path()).unwrap();
    let config = ServerConfig {
        poll_interval: Duration::from_millis(10),
        heartbeat: Duration::from_millis(20),
        ..ServerConfig::default()
    };
    let server = TestServer::start(dir.path(), config);

    let mut reader = open_events(server.addr, "/api/v1/events?epoch=1&after=0", None);
    let frame = reader.read_frame().expect("heartbeat frame");
    assert_eq!(frame.id, None, "a heartbeat must not carry an id");
    assert_eq!(frame.event, None, "a heartbeat is a comment");
    assert_eq!(frame.data, None, "a heartbeat carries no data");
}

#[test]
fn a_midstream_epoch_change_requires_reset_and_closes() {
    let dir = project();
    let service = SnapshotService::open(dir.path()).unwrap();
    let head = append_mission(&service, 1);
    let server = TestServer::start(dir.path(), ServerConfig::default());
    let mut reader = open_events(
        server.addr,
        &format!("/api/v1/events?epoch={}&after={}", head.epoch, head.seq),
        None,
    );

    let snapshot = service.snapshot().unwrap();
    SnapshotService::begin_new_epoch_after_continuity_loss(
        dir.path(),
        snapshot,
        "test asserted continuity loss",
    )
    .unwrap();

    let frame = reader.read_frame().expect("reset control frame");
    assert_eq!(frame.event.as_deref(), Some("reset_required"));
    assert_eq!(frame.id, None);
    let data: Value = serde_json::from_str(frame.data.as_deref().unwrap()).unwrap();
    assert_eq!(data["error"]["code"], "wrong_epoch");
    assert_eq!(data["current_cursor"]["epoch"], 2);
}

// -- resources ---------------------------------------------------------------

#[test]
fn resources_route_lists_the_authoritative_registry() {
    let dir = project();
    let service = SnapshotService::open(dir.path()).unwrap();
    let mut registry = ResourceRegistry::new(1);
    let identity = ResourceIdentity::for_model("openai", "gpt-5");
    registry.observe_available(&identity, "probe", 1);
    resources::save(dir.path(), &registry).unwrap();
    assert!(service.head().unwrap().seq >= 1);
    let server = TestServer::start(dir.path(), ServerConfig::default());

    let response = raw_request(server.addr, "GET", "/api/v1/resources", None, &[]);
    assert_eq!(response.status, 200);
    let value = response.json();
    assert_eq!(value["resources"].as_array().unwrap().len(), 1);
    assert_eq!(value["corrupt"], false);
}

// -- protocol errors ---------------------------------------------------------

#[test]
fn unknown_routes_and_methods_are_typed() {
    let dir = project();
    SnapshotService::open(dir.path()).unwrap();
    let server = TestServer::start(dir.path(), ServerConfig::default());

    let missing = raw_request(server.addr, "GET", "/api/v1/nope", None, &[]);
    assert_eq!(missing.status, 404);
    assert_eq!(missing.json()["error"]["code"], "not_found");

    let method = raw_request(server.addr, "POST", "/api/v1/snapshot", None, &[]);
    assert_eq!(method.status, 405);
    let allow = method
        .headers
        .iter()
        .find(|(name, _)| name == "allow")
        .map(|(_, value)| value.as_str())
        .unwrap_or_default();
    assert_eq!(allow, "GET");
}

#[test]
fn malformed_and_oversize_requests_are_rejected() {
    let dir = project();
    SnapshotService::open(dir.path()).unwrap();
    let server = TestServer::start(dir.path(), ServerConfig::default());

    let malformed = send_and_read(server.addr, "GARBAGE\r\n\r\n");
    assert_eq!(malformed.status, 400);
    assert_eq!(malformed.json()["error"]["code"], "malformed_request");

    let oversize_length = (MAX_BODY_BYTES + 1).to_string();
    let oversized = raw_request(
        server.addr,
        "POST",
        "/api/v1/approvals/apr-missing/approve",
        None,
        &[("Content-Length", oversize_length.as_str())],
    );
    assert_eq!(oversized.status, 413);
    assert_eq!(oversized.json()["error"]["code"], "payload_too_large");

    let bad_json = raw_request(
        server.addr,
        "POST",
        "/api/v1/approvals/apr-missing/approve",
        Some("not-json"),
        &[],
    );
    assert_eq!(bad_json.status, 400);
    assert_eq!(bad_json.json()["error"]["code"], "malformed_json");

    let huge_value = "x".repeat(opencode_gear::control_server::MAX_HEADER_BYTES + 1);
    let oversized_head = send_and_read(
        server.addr,
        &format!(
            "GET /api/v1/snapshot HTTP/1.1\r\nHost: localhost\r\nX-Large: {huge_value}\r\n\r\n"
        ),
    );
    assert_eq!(oversized_head.status, 431);
    assert_eq!(oversized_head.json()["error"]["code"], "headers_too_large");

    let duplicate_length = send_and_read(
        server.addr,
        "POST /api/v1/approvals/apr-missing/approve HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
    );
    assert_eq!(duplicate_length.status, 400);
    assert_eq!(
        duplicate_length.json()["error"]["code"],
        "malformed_request"
    );

    let trailing = send_and_read(
        server.addr,
        "POST /api/v1/approvals/apr-missing/approve HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n{}",
    );
    assert_eq!(trailing.status, 400);
    assert_eq!(trailing.json()["error"]["code"], "malformed_request");
}

#[test]
fn bind_refuses_non_loopback_addresses() {
    let dir = project();
    for refused in ["0.0.0.0:0", "192.168.1.10:0", "[::]:0", "not-an-address"] {
        assert!(
            ControlServer::bind(refused, dir.path(), ServerConfig::default()).is_err(),
            "{refused} must be refused"
        );
    }
    // A loopback bind still works and reports a loopback address.
    let server = ControlServer::bind("127.0.0.1:0", dir.path(), ServerConfig::default()).unwrap();
    assert!(server.local_addr().ip().is_loopback());
    assert!(server.base_url().starts_with("http://127.0.0.1:"));

    // An invalid server configuration fails before binding.
    let config = ServerConfig {
        max_clients: 0,
        ..ServerConfig::default()
    };
    assert!(ControlServer::bind("127.0.0.1:0", dir.path(), config).is_err());
}

/// A cross-process-shaped check: the mutation is applied by the server process
/// while another handle observes the durable authority afterward.
#[test]
fn a_mutation_is_visible_to_an_independent_authority_reader() {
    let dir = project();
    let reader = SnapshotService::open(dir.path()).unwrap();
    let admitted = mission(1, 1);
    let mission_id = admitted.mission_id.clone();
    mission::save(dir.path(), &admitted).unwrap();
    let server = TestServer::start(dir.path(), ServerConfig::default());

    let put = raw_request(
        server.addr,
        "PUT",
        &format!("/api/v1/budgets/{mission_id}"),
        Some("{\"limit_micros\":123456,\"currency\":\"USD\"}"),
        &[],
    );
    assert_eq!(put.status, 200);

    let reloaded = mission::load(dir.path(), &mission_id).unwrap().unwrap();
    assert_eq!(reloaded.budget.receipt().hard_limit_micros, Some(123456));
    assert!(reader.head().unwrap().seq >= 2);
}

#[test]
fn sse_observes_a_real_cli_process_mutation() {
    let dir = project();
    std::fs::write(dir.path().join(".opencode-gear.yaml"), "{}\n").unwrap();
    let admitted = mission(1, 1);
    let mission_id = admitted.mission_id.clone();
    mission::save(dir.path(), &admitted).unwrap();
    let authority = SnapshotService::open(dir.path()).unwrap();
    let head = authority.head().unwrap();
    let server = TestServer::start(dir.path(), ServerConfig::default());
    let mut reader = open_events(
        server.addr,
        &format!("/api/v1/events?epoch={}&after={}", head.epoch, head.seq),
        None,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_ocg"))
        .current_dir(dir.path())
        .env("OPENCODE_GEAR_USER_CONFIG", dir.path().join("no-user.yaml"))
        .env_remove("OPENCODE_GEAR_PROJECT_CONFIG")
        .args([
            "budget",
            "set",
            "--mission",
            &mission_id,
            "--limit",
            "321000",
            "--currency",
            "USD",
        ])
        .output()
        .expect("run the independent ocg mutation");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let frame = reader.read_frame().expect("cross-process journal event");
    assert_eq!(frame.event.as_deref(), Some("mission_upsert"));
    let expected_id = format!("1:{}", head.seq + 1);
    assert_eq!(frame.id.as_deref(), Some(expected_id.as_str()));
    let event: Value = serde_json::from_str(frame.data.as_deref().unwrap()).unwrap();
    assert_eq!(
        event["event"]["mission"]["budget"]["hard_limit"]["micros"],
        321000
    );
    let snapshot = authority.snapshot().unwrap();
    assert_eq!(
        snapshot.missions[&mission_id]
            .budget
            .receipt()
            .hard_limit_micros,
        Some(321000)
    );
}

#[test]
fn corrupt_authority_is_an_explicit_service_failure() {
    let dir = project();
    SnapshotService::open(dir.path()).unwrap();
    let server = TestServer::start(dir.path(), ServerConfig::default());
    std::fs::write(opencode_gear::orchestration::state_path(dir.path()), b"{}").unwrap();

    let snapshot = raw_request(server.addr, "GET", "/api/v1/snapshot", None, &[]);
    assert_eq!(snapshot.status, 503);
    assert_eq!(snapshot.json()["error"]["code"], "persistence_unavailable");
    let events = raw_request(
        server.addr,
        "GET",
        "/api/v1/events?epoch=1&after=0",
        None,
        &[],
    );
    assert_eq!(events.status, 503);
    assert_eq!(events.json()["error"]["code"], "persistence_unavailable");
}
