// Shared, loopback-only browser acceptance fixture. Compiled only by test modules.
use std::collections::VecDeque;
use std::net::TcpStream;
use std::sync::{mpsc::SyncSender, Arc};
use std::time::{Duration, Instant};

const PROOF_WAIT: Duration = Duration::from_secs(15);
const MAX_PRECONNECTIONS: usize = 64;

pub(crate) type CapturedProof = (String, String, serde_json::Value);

#[derive(Debug, Default)]
struct DocumentGate {
    state: std::sync::Mutex<(bool, bool)>,
    changed: std::sync::Condvar,
}
impl DocumentGate {
    fn hold(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        state.0 = true;
        self.changed.notify_all();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !state.1 {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return false;
            };
            state = self.changed.wait_timeout(state, remaining).unwrap().0;
        }
        true
    }
    fn wait_requested(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while !state.0 {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return false;
            };
            state = self.changed.wait_timeout(state, remaining).unwrap().0;
        }
        true
    }
    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }
}

#[derive(Debug, Default)]
struct FixtureTrace {
    events: std::sync::Mutex<VecDeque<(u128, u64, &'static str)>>,
}
impl FixtureTrace {
    fn record(&self, id: u64, stage: &'static str) {
        let mut events = self.events.lock().unwrap();
        if events.len() == 128 {
            events.pop_front();
        }
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        events.push_back((time.as_millis(), id, stage));
    }

    fn dump(&self) {
        let events = self.events.lock().unwrap();
        eprintln!("loopback fixture trace (Unix ms, connection, stage): {events:?}");
    }
}

struct Preconnection {
    id: u64,
    stream: TcpStream,
    accepted: Instant,
}

fn dispatch_readable(
    pending: &mut VecDeque<Preconnection>,
    send: &SyncSender<(u64, TcpStream)>,
    trace: &FixtureTrace,
) {
    use std::io::ErrorKind;

    for _ in 0..pending.len() {
        let connection = pending.pop_front().unwrap();
        let id = connection.id;
        // Peek leaves the first byte in the socket for the HTTP reader.
        match connection.stream.peek(&mut [0]) {
            Ok(0) => trace.record(id, "preconnect-eof"),
            Ok(_) => {
                trace.record(id, "first-byte-ready");
                if send.try_send((id, connection.stream)).is_err() {
                    trace.record(id, "queue-rejected");
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
                if connection.accepted.elapsed() < PROOF_WAIT {
                    pending.push_back(connection);
                } else {
                    trace.record(id, "preconnect-timeout");
                }
            }
            Err(_) => trace.record(id, "preconnect-error"),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ControlFixture {
    pub(crate) url: String,
    pub(crate) proofs: Arc<std::sync::Mutex<Vec<CapturedProof>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    service_version: Arc<std::sync::atomic::AtomicUsize>,
    modules: bool,
    trace: Arc<FixtureTrace>,
    document_gate: Arc<DocumentGate>,
}
impl ControlFixture {
    pub(crate) fn wait_held_document(&self) -> bool {
        self.document_gate.wait_requested()
    }
    pub(crate) fn release_document(&self) {
        self.document_gate.release();
    }
    pub(crate) fn bump_service_version(&self) {
        self.service_version
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
    pub(crate) fn wait_kind_count(
        &self,
        prefix: &str,
        kind: &str,
        count: usize,
    ) -> Vec<CapturedProof> {
        let deadline = Instant::now() + PROOF_WAIT;
        loop {
            let proofs: Vec<_> = self
                .proofs
                .lock()
                .unwrap()
                .iter()
                .filter(|(path, _, first)| path.starts_with(prefix) && first["kind"] == kind)
                .cloned()
                .collect();
            if proofs.len() >= count {
                return proofs;
            }
            if std::time::Instant::now() >= deadline {
                self.trace.dump();
                return proofs;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    pub(crate) fn missing_proof_kinds(&self, proofs: &[CapturedProof]) -> Vec<&'static str> {
        let classic = [
            "page",
            "frame",
            "cross_frame",
            "worker",
            "shared_worker",
            "service_worker",
        ];
        let modules: &[&str] = if self.modules {
            &[
                "module_worker",
                "module_shared_worker",
                "module_service_worker",
            ]
        } else {
            &[]
        };
        classic
            .iter()
            .chain(modules)
            .copied()
            .filter(|kind| !proofs.iter().any(|(_, _, value)| value["kind"] == *kind))
            .collect()
    }

    pub(crate) fn wait_proofs(&self, prefix: &str) -> Vec<CapturedProof> {
        let deadline = Instant::now() + PROOF_WAIT;
        loop {
            let proofs: Vec<_> = self
                .proofs
                .lock()
                .unwrap()
                .iter()
                .filter(|(path, _, _)| path.starts_with(prefix))
                .cloned()
                .collect();
            if self.missing_proof_kinds(&proofs).is_empty() {
                return proofs;
            }
            if std::time::Instant::now() >= deadline {
                self.trace.dump();
                return proofs;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn serve(
        mut stream: std::net::TcpStream,
        output: &std::sync::Mutex<Vec<CapturedProof>>,
        cross_origin: &str,
        service_version: &std::sync::atomic::AtomicUsize,
        modules: bool,
        document_gate: &DocumentGate,
        (trace, id): (&FixtureTrace, u64),
    ) {
        use std::io::{Read, Write};
        // Accepted sockets inherit FIONBIO on Windows. Switch only this
        // connection back to blocking I/O before applying bounded timeouts.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 2048];
        trace.record(id, "reading");
        let mut headers_seen = false;
        let request = loop {
            match stream.read(&mut buffer) {
                Ok(0) => {
                    trace.record(id, "eof");
                    break None;
                }
                Err(error) => {
                    use std::io::ErrorKind;
                    let stage = match error.kind() {
                        ErrorKind::Interrupted => continue,
                        ErrorKind::TimedOut | ErrorKind::WouldBlock => "read-timeout",
                        _ => "read-error",
                    };
                    trace.record(id, stage);
                    break None;
                }
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
            }
            if bytes.len() > 16384 {
                trace.record(id, "request-too-large");
                break None;
            }
            if let Some(end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&bytes[..end]).to_string();
                if !headers_seen {
                    let document = head
                        .split_whitespace()
                        .nth(1)
                        .is_some_and(|path| path.ends_with("/page"));
                    trace.record(
                        id,
                        if document {
                            "document-headers"
                        } else {
                            "headers"
                        },
                    );
                    headers_seen = true;
                }
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if length > 16384 - (end + 4) {
                    trace.record(id, "request-too-large");
                    break None;
                }
                if bytes.len() >= end + 4 + length {
                    break Some((head, bytes[end + 4..end + 4 + length].to_vec()));
                }
            }
        };
        let Some((headers, body)) = request else {
            return;
        };
        let path = headers.split_whitespace().nth(1).unwrap_or("/").to_owned();
        if path == "/closed/page" {
            trace.record(id, "intentional-close");
            output.lock().unwrap().push((
                path,
                String::new(),
                serde_json::json!({"kind":"closed-response-fixture"}),
            ));
            return;
        }
        if path == "/held/page" && !document_gate.hold() {
            return;
        }
        if let Some(route) = path.strip_prefix("/soak/") {
            let (index, heartbeat) = route
                .strip_suffix("/heartbeat")
                .map_or((route, false), |index| (index, true));
            if let Ok(index) = index.parse::<usize>() {
                if index < 11 && (!heartbeat || headers.starts_with("POST ")) {
                    let key = if heartbeat {
                        if body == b"canvas-changed" {
                            format!("/soak/{index}/canvas-changed")
                        } else {
                            format!("/soak/{index}/heartbeat")
                        }
                    } else {
                        format!("/soak/{index}/document")
                    };
                    {
                        let mut proofs = output.lock().unwrap();
                        if let Some((_, _, count)) =
                            proofs.iter_mut().find(|(path, _, _)| path == &key)
                        {
                            *count = serde_json::json!(count.as_u64().unwrap() + 1);
                        } else {
                            proofs.push((key, String::new(), serde_json::json!(1)));
                        }
                    }
                    let (kind, content) = if heartbeat {
                        ("text/plain", "ok")
                    } else {
                        (
                            "text/html",
                            include_str!(concat!(
                                env!("CARGO_MANIFEST_DIR"),
                                "/../../tools/test_support/soak_page.html"
                            )),
                        )
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{content}",
                        content.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    return;
                }
            }
        }
        println!("local acceptance request: {path}");
        let revision = service_version.load(std::sync::atomic::Ordering::Acquire);
        let script = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/test_support/first_observation.js"
        ))
        .replace("__BROWS_FIXTURE_VERSION__", &revision.to_string())
        .replace(
            "__BROWS_GPU_OBSERVATION__",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tools/test_support/webgl_observation.js"
            )),
        )
        .replace(
            "__BROWS_FIXTURE_PHASE__",
            if path.ends_with("dependency.mjs") {
                "module_dependency"
            } else {
                "first_script"
            },
        );
        let (content, kind) = if path.ends_with("dependency.mjs") {
            (
                format!("{script}\nexport {{first,proof}};"),
                "text/javascript",
            )
        } else if path.ends_with("shared.mjs") {
            (
                "import './dependency.mjs';onconnect=event=>event.ports[0].start();".to_owned(),
                "text/javascript",
            )
        } else if path.ends_with("service.mjs") {
            ("import {proof} from './dependency.mjs';addEventListener('install',event=>event.waitUntil(Promise.all([proof,skipWaiting()])));addEventListener('activate',event=>event.waitUntil(clients.claim()));addEventListener('message',event=>event.waitUntil(proof));".to_owned(), "text/javascript")
        } else if path.ends_with("worker.mjs") {
            (
                "import './dependency.mjs';setInterval(()=>{},1000);".to_owned(),
                "text/javascript",
            )
        } else if path.ends_with("shared.js") {
            (
                format!("{script}onconnect=event=>event.ports[0].start();"),
                "text/javascript",
            )
        } else if path.ends_with("service.js") {
            (
                    format!("{script}addEventListener('install',event=>event.waitUntil(Promise.all([proof,skipWaiting()])));addEventListener('activate',event=>event.waitUntil(clients.claim()));addEventListener('message',event=>event.waitUntil(proof));\n//service-version-{}", service_version.load(std::sync::atomic::Ordering::Acquire)),
                "text/javascript",
            )
        } else if path.ends_with("worker.js") {
            (
                format!("{script}setInterval(()=>{{}},1000);"),
                "text/javascript",
            )
        } else if path.ends_with("/version") {
            (revision.to_string(), "application/json")
        } else if path.ends_with("proof") {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) {
                println!(
                    "local first-script sample: {} CPU {} timezone {}",
                    value["kind"], value["hardwareConcurrency"], value["timezone"]
                );
                output.lock().unwrap().push((path.clone(), headers, value));
            }
            ("ok".to_owned(), "text/plain")
        } else if path.ends_with("error") {
            println!(
                "local acceptance script error: {}",
                String::from_utf8_lossy(&body)
            );
            ("ok".to_owned(), "text/plain")
        } else if path.starts_with("/live-network/") && path.ends_with("/page") {
            let workload = include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tools/test_support/live_network_page.js"
            ));
            (
                format!("<!doctype html><meta charset='utf-8'><script>{workload}</script>"),
                "text/html",
            )
        } else if path.starts_with("/seeded-soak/") && path.ends_with("/page") {
            let workload = include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tools/test_support/seeded_soak_page.js"
            ));
            (
                format!("<!doctype html><meta charset='utf-8'><script>{script}{workload}</script>"),
                "text/html",
            )
        } else {
            let worker = if path.ends_with("page") {
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tools/test_support/fixture_page.js"
                ))
                .replace(
                    "__BROWS_FIXTURE_MODULES__",
                    if modules { "true" } else { "false" },
                )
            } else {
                String::new()
            };
            let frame = if path.ends_with("page") {
                format!(
                    "<iframe src='./frame'></iframe><iframe src='{cross_origin}{}frame'></iframe>",
                    path.trim_end_matches("page")
                )
            } else {
                String::new()
            };
            (
                format!(
                    "<!doctype html><meta charset='utf-8'><script>{script}{worker}</script>{frame}"
                ),
                "text/html",
            )
        };
        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nAccept-CH: Sec-CH-UA-Platform, Sec-CH-UA-Full-Version-List\r\n\r\n{content}", content.len());
        match stream.write_all(response.as_bytes()) {
            Ok(()) if path.ends_with("/page") => trace.record(id, "document-response-written"),
            Err(_) => trace.record(id, "response-write-error"),
            _ => {}
        }
    }

    pub(crate) fn start(modules: bool) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let listener_v6 =
            std::net::TcpListener::bind(("::1", listener.local_addr().unwrap().port())).unwrap();
        let cross_origin = format!("http://localhost:{}", listener.local_addr().unwrap().port());
        listener.set_nonblocking(true).unwrap();
        listener_v6.set_nonblocking(true).unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let proofs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let halt = stop.clone();
        let output = proofs.clone();
        let service_version = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let version = service_version.clone();
        let document_gate = Arc::new(DocumentGate::default());
        let gate = document_gate.clone();
        let trace = Arc::new(FixtureTrace::default());
        let server_trace = trace.clone();
        let thread = std::thread::spawn(move || {
            // Browsers preconnect before sending headers. One idle socket
            // must not block or prematurely close unrelated script loads.
            let (send, receive) = std::sync::mpsc::sync_channel::<(u64, std::net::TcpStream)>(64);
            let receive = Arc::new(std::sync::Mutex::new(receive));
            std::thread::scope(|scope| {
                for _ in 0..16 {
                    let receive = receive.clone();
                    let output = output.clone();
                    let halt = halt.clone();
                    let cross_origin = &cross_origin;
                    let version = version.clone();
                    let gate = gate.clone();
                    let trace = &server_trace;
                    scope.spawn(move || {
                        while !halt.load(std::sync::atomic::Ordering::Acquire) {
                            let next = receive
                                .lock()
                                .unwrap()
                                .recv_timeout(std::time::Duration::from_millis(100));
                            match next {
                                Ok((id, stream)) => Self::serve(
                                    stream,
                                    &output,
                                    cross_origin,
                                    &version,
                                    modules,
                                    &gate,
                                    (trace, id),
                                ),
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                                Err(_) => break,
                            }
                        }
                    });
                }
                let mut id = 0;
                let mut pending = VecDeque::new();
                while !halt.load(std::sync::atomic::Ordering::Acquire) {
                    let accepted = listener.accept().or_else(|error| {
                        if error.kind() == std::io::ErrorKind::WouldBlock {
                            listener_v6.accept()
                        } else {
                            Err(error)
                        }
                    });
                    match accepted {
                        Ok((stream, peer)) => {
                            id += 1;
                            let stage = if peer.is_ipv4() {
                                "accept-v4"
                            } else {
                                "accept-v6"
                            };
                            server_trace.record(id, stage);
                            if pending.len() >= MAX_PRECONNECTIONS {
                                server_trace.record(id, "preconnect-limit");
                            } else if stream.set_nonblocking(true).is_err() {
                                server_trace.record(id, "preconnect-error");
                            } else {
                                pending.push_back(Preconnection {
                                    id,
                                    stream,
                                    accepted: Instant::now(),
                                });
                            }
                        }
                        Err(error) => {
                            if error.kind() != std::io::ErrorKind::WouldBlock {
                                server_trace.record(0, "accept-error");
                            }
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                    }
                    dispatch_readable(&mut pending, &send, &server_trace);
                }
                drop(send);
            });
        });
        Self {
            url,
            proofs,
            stop,
            thread: Some(thread),
            service_version,
            modules,
            trace,
            document_gate,
        }
    }
}
impl Drop for ControlFixture {
    fn drop(&mut self) {
        self.document_gate.release();
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{atomic::AtomicUsize, mpsc, Mutex};
    use std::time::Duration;

    #[test]
    fn held_document_waits_for_explicit_release() {
        let fixture = ControlFixture::start(false);
        let address = fixture.url.strip_prefix("http://").unwrap().to_owned();
        let client = std::thread::spawn(move || {
            let mut client = TcpStream::connect(address).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            client
                .write_all(b"GET /held/page HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            response
        });
        assert!(fixture.wait_held_document());
        assert!(fixture.proofs.lock().unwrap().is_empty());
        fixture.release_document();
        assert!(client.join().unwrap().starts_with("HTTP/1.1 200 OK\r\n"));
        let events = fixture.trace.events.lock().unwrap();
        let request = events.iter().find(|e| e.2 == "document-headers").unwrap();
        assert!(events
            .iter()
            .any(|e| e.1 == request.1 && e.2 == "accept-v4"));
        assert!(events
            .iter()
            .any(|e| e.1 == request.1 && e.2 == "document-response-written"));
    }

    #[test]
    fn preconnections_wait_for_request_bytes_without_occupying_http_workers() {
        let fixture = ControlFixture::start(false);
        let address = fixture.url.strip_prefix("http://").unwrap();
        let mut idle: Vec<_> = (0..16)
            .map(|_| TcpStream::connect(address).unwrap())
            .collect();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let events = fixture.trace.events.lock().unwrap();
            if events.iter().filter(|e| e.2 == "accept-v4").count() == idle.len() {
                assert!(!events.iter().any(|e| e.2 == "reading"));
                break;
            }
            drop(events);
            assert!(
                Instant::now() < deadline,
                "fixture did not accept idle sockets"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        let request = |mut socket: TcpStream| {
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            socket
                .write_all(b"GET /version HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .unwrap();
            let mut response = String::new();
            socket.read_to_string(&mut response).unwrap();
            assert!(response.ends_with("\r\n\r\n0"));
        };
        request(TcpStream::connect(address).unwrap());
        // The HTTP reader has a 3s timeout once a request begins.
        std::thread::sleep(Duration::from_millis(3300));
        request(idle.pop().unwrap());
    }

    #[test]
    fn fixture_accepts_delayed_headers_on_a_nonblocking_accepted_socket() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (send, ready) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_nonblocking(true).unwrap();
            send.send(()).unwrap();
            ControlFixture::serve(
                stream,
                &Mutex::new(Vec::new()),
                "http://localhost",
                &AtomicUsize::new(0),
                false,
                &DocumentGate::default(),
                (&FixtureTrace::default(), 1),
            );
        });
        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        client
            .write_all(b"GET /version HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        server.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.ends_with("\r\n\r\n0"));
    }
}
