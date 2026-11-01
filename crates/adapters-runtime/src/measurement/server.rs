use super::*;
use serde::Deserialize;
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Mutex,
    },
    thread::{self, JoinHandle},
};

const HEADER_LIMIT: usize = 8192;
const BODY_LIMIT: usize = 64 * 1024;
const SCRIPT: &str = include_str!("capture.js");

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RawStorage {
    pub local_available: bool,
    pub cookie_available: bool,
    pub local_before: Option<String>,
    pub local_after: Option<String>,
    pub cookie_before: Option<String>,
    pub cookie_after: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct BrowserObservation {
    token: String,
    run_id: String,
    pub page: FingerprintSignalsDto,
    pub iframe: Option<FingerprintSignalsDto>,
    pub worker: Option<FingerprintSignalsDto>,
    pub audio: Option<FingerprintAudioDto>,
    pub storage: RawStorage,
}
#[derive(Debug, Clone)]
pub(super) struct RawObservation {
    pub browser: BrowserObservation,
    pub network: FingerprintHeadersDto,
}
#[derive(Debug, Default)]
struct RunState {
    headers: Option<FingerprintHeadersDto>,
    observation: Option<RawObservation>,
    collected: bool,
}
#[derive(Debug)]
struct Shared {
    token: String,
    origin: String,
    stopped: AtomicBool,
    runs: Mutex<BTreeMap<String, RunState>>,
}

#[derive(Debug)]
pub(super) struct Collector {
    address: SocketAddr,
    shared: Arc<Shared>,
    sender: Option<mpsc::SyncSender<TcpStream>>,
    listener: Option<JoinHandle<()>>,
    workers: Vec<JoinHandle<()>>,
}
impl Collector {
    pub fn start(token: &str) -> PortResult<Self> {
        if token.len() != 32 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid_data("invalid observation token"));
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .map_err(|_| internal("无法启动本地观测页面"))?;
        listener
            .set_nonblocking(true)
            .map_err(|_| internal("无法配置本地观测服务器"))?;
        let address = listener
            .local_addr()
            .map_err(|_| internal("无法读取本地观测端口"))?;
        let shared = Arc::new(Shared {
            token: token.into(),
            origin: format!("http://{address}"),
            stopped: AtomicBool::new(false),
            runs: Mutex::new(
                ["a0", "b0", "a1", "b1"]
                    .into_iter()
                    .map(|id| (id.into(), RunState::default()))
                    .collect(),
            ),
        });
        let (sender, receiver) = mpsc::sync_channel::<TcpStream>(8);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut server = Self {
            address,
            shared,
            sender: Some(sender),
            listener: None,
            workers: Vec::new(),
        };
        for _ in 0..4 {
            let receiver = receiver.clone();
            let shared = server.shared.clone();
            server.workers.push(
                thread::Builder::new()
                    .name("fingerprint-observe-http".into())
                    .spawn(move || loop {
                        let next = receiver.lock().unwrap_or_else(|e| e.into_inner()).recv();
                        let Ok(mut stream) = next else {
                            break;
                        };
                        if shared.stopped.load(Ordering::Acquire) {
                            continue;
                        }
                        serve(&mut stream, &shared);
                    })
                    .map_err(|_| internal("无法启动本地观测工作线程"))?,
            );
        }
        let sender = server.sender.as_ref().unwrap().clone();
        let shared = server.shared.clone();
        server.listener = Some(
            thread::Builder::new()
                .name("fingerprint-observe-listener".into())
                .spawn(move || {
                    while !shared.stopped.load(Ordering::Acquire) {
                        match listener.accept() {
                            Ok((stream, peer)) if peer.ip().is_loopback() => {
                                let _ = sender.try_send(stream);
                            }
                            Ok(_) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(5))
                            }
                            Err(_) => break,
                        }
                    }
                })
                .map_err(|_| internal("无法启动本地观测监听器"))?,
        );
        Ok(server)
    }
    pub fn url(&self, run_id: &str) -> String {
        format!(
            "http://{}/{}/{run_id}/page",
            self.address, self.shared.token
        )
    }
    pub fn take(&self, run_id: &str) -> PortResult<Option<RawObservation>> {
        Ok(self
            .shared
            .runs
            .lock()
            .map_err(|_| internal("本地观测结果不可用"))?
            .get_mut(run_id)
            .and_then(|run| run.observation.take()))
    }
}
impl Drop for Collector {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        self.sender.take();
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
fn read_request(stream: &mut TcpStream) -> Option<Request> {
    stream
        .set_read_timeout(Some(Duration::from_millis(150)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_millis(200)))
        .ok()?;
    let deadline = Instant::now() + Duration::from_millis(700);
    let mut data = Vec::new();
    let mut chunk = [0u8; 2048];
    let end = loop {
        if Instant::now() >= deadline || data.len() > HEADER_LIMIT + BODY_LIMIT {
            return None;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&chunk[..n]);
        if let Some(end) = data.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break end + 4;
        }
        if data.len() > HEADER_LIMIT {
            return None;
        }
    };
    if end > HEADER_LIMIT {
        return None;
    }
    let text = std::str::from_utf8(&data[..end]).ok()?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    if first.next()? != "HTTP/1.1"
        || first.next().is_some()
        || !matches!(method.as_str(), "GET" | "POST")
    {
        return None;
    }
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':')?;
        let name = name.to_ascii_lowercase();
        if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || headers.insert(name, value.trim().to_owned()).is_some()
        {
            return None;
        }
    }
    if headers.contains_key("transfer-encoding") {
        return None;
    }
    let length = headers
        .get("content-length")
        .map(|v| v.parse::<usize>())
        .transpose()
        .ok()?
        .unwrap_or(0);
    if length > BODY_LIMIT || method == "GET" && length != 0 {
        return None;
    }
    while data.len() < end + length {
        if Instant::now() >= deadline {
            return None;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&chunk[..n]);
        if data.len() > HEADER_LIMIT + BODY_LIMIT {
            return None;
        }
    }
    if data.len() != end + length {
        return None;
    }
    Some(Request {
        method,
        path,
        headers,
        body: data[end..].to_vec(),
    })
}
fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) {
    let header = format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nCross-Origin-Resource-Policy: same-origin\r\nContent-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; worker-src 'self'; frame-src 'self'; img-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'\r\n\r\n", body.len());
    let _ = stream
        .write_all(header.as_bytes())
        .and_then(|()| stream.write_all(body));
}
fn serve(stream: &mut TcpStream, shared: &Shared) {
    let Some(request) = read_request(stream) else {
        return;
    };
    let host = shared.origin.strip_prefix("http://").unwrap_or_default();
    if request.headers.get("host").map(String::as_str) != Some(host) {
        respond(stream, "403 Forbidden", "text/plain", b"");
        return;
    }
    let prefix = format!("/{}/", shared.token);
    let Some((run_id, route)) = request
        .path
        .strip_prefix(&prefix)
        .and_then(|p| p.split_once('/'))
    else {
        respond(stream, "404 Not Found", "text/plain", b"");
        return;
    };
    if !["a0", "b0", "a1", "b1"].contains(&run_id) {
        respond(stream, "404 Not Found", "text/plain", b"");
        return;
    }
    if request.method == "POST" && route == "collect" {
        if request.headers.get("origin").map(String::as_str) != Some(shared.origin.as_str())
            || request
                .headers
                .get("content-type")
                .and_then(|v| v.split(';').next())
                .map(str::trim)
                != Some("application/json")
        {
            respond(stream, "403 Forbidden", "text/plain", b"");
            return;
        }
        let Ok(browser) = decode(&request.body, &shared.token, run_id) else {
            respond(stream, "400 Bad Request", "text/plain", b"");
            return;
        };
        let mut runs = shared.runs.lock().unwrap_or_else(|e| e.into_inner());
        let run = runs.get_mut(run_id).unwrap();
        if run.collected || run.headers.is_none() {
            respond(stream, "409 Conflict", "text/plain", b"");
            return;
        }
        run.observation = Some(RawObservation {
            browser,
            network: run.headers.clone().unwrap(),
        });
        run.collected = true;
        drop(runs);
        respond(stream, "204 No Content", "application/json", b"");
        return;
    }
    if request.method != "GET" {
        respond(stream, "404 Not Found", "text/plain", b"");
        return;
    }
    match route {
        "page" | "frame" | "worker.js" => {
            let kind = if route == "page" {
                "page"
            } else if route == "frame" {
                "frame"
            } else {
                "worker"
            };
            if kind == "page" {
                let only = |key: &str| request.headers.get(key).cloned();
                let headers = FingerprintHeadersDto {
                    user_agent: only("user-agent"),
                    accept_language: only("accept-language"),
                    sec_ch_ua: only("sec-ch-ua"),
                    sec_ch_ua_platform: only("sec-ch-ua-platform"),
                };
                shared
                    .runs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_mut(run_id)
                    .unwrap()
                    .headers
                    .get_or_insert(headers);
                // Hold both documents until both browser processes have reached
                // this origin. Compiling/verifying B may take longer than A's
                // entire render; spawning in a loop alone does not prove overlap.
                let peer = format!(
                    "{}{}",
                    if run_id.starts_with('a') { "b" } else { "a" },
                    &run_id[1..]
                );
                let deadline = Instant::now() + Duration::from_secs(22);
                loop {
                    if shared.stopped.load(Ordering::Acquire) || Instant::now() >= deadline {
                        respond(stream, "408 Request Timeout", "text/plain", b"");
                        return;
                    }
                    if shared
                        .runs
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&peer)
                        .is_some_and(|run| run.headers.is_some())
                    {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            }
            let body = document(&shared.token, run_id, kind);
            respond(
                stream,
                "200 OK",
                if kind == "worker" {
                    "text/javascript; charset=utf-8"
                } else {
                    "text/html; charset=utf-8"
                },
                body.as_bytes(),
            );
        }
        "finish.svg" => {
            let deadline = Instant::now() + Duration::from_secs(22);
            while !shared.stopped.load(Ordering::Acquire) && Instant::now() < deadline {
                if shared
                    .runs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(run_id)
                    .is_some_and(|run| run.collected)
                {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            respond(
                stream,
                "200 OK",
                "image/svg+xml",
                br#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>"#,
            );
        }
        _ => respond(stream, "404 Not Found", "text/plain", b""),
    }
}
fn document(token: &str, run_id: &str, kind: &str) -> String {
    let base = format!("/{token}/{run_id}");
    let config = serde_json::json!({"token":token,"runId":run_id,"kind":kind,"base":base,"marker":format!("{token}-{}", &run_id[..1])});
    let script = SCRIPT.replace("__MEASUREMENT_CONFIG__", &config.to_string());
    if kind == "worker" {
        return script;
    }
    // The image keeps load pending until the independently collected report is
    // accepted. --dump-dom can then exit Chrome normally and flush profile data.
    let barrier = if kind == "page" {
        format!("<img alt=\"\" width=\"1\" height=\"1\" src=\"{base}/finish.svg\">")
    } else {
        String::new()
    };
    format!("<!doctype html><html><head><meta charset=\"utf-8\"><script>{script}</script></head><body>{barrier}</body></html>")
}
pub(super) fn decode(data: &[u8], token: &str, run_id: &str) -> PortResult<BrowserObservation> {
    if data.len() > BODY_LIMIT {
        return Err(invalid_data("observation exceeds limit"));
    }
    let value: BrowserObservation =
        serde_json::from_slice(data).map_err(|_| invalid_data("浏览器观测格式无效"))?;
    if value.token != token || value.run_id != run_id {
        return Err(invalid_data("观测身份与当前运行不符"));
    }
    for signal in std::iter::once(&value.page)
        .chain(value.iframe.iter())
        .chain(value.worker.iter())
    {
        validate_signal(signal)?;
    }
    if let Some(audio) = &value.audio {
        for hash in [
            &audio.first_hash,
            &audio.repeat_hash,
            &audio.first_event_hash,
            &audio.repeat_event_hash,
        ] {
            validate_hash(hash)?;
        }
    }
    for text in [
        &value.storage.local_before,
        &value.storage.local_after,
        &value.storage.cookie_before,
        &value.storage.cookie_after,
    ]
    .into_iter()
    .flatten()
    {
        if text.len() > 128 {
            return Err(invalid_data("invalid storage observation"));
        }
    }
    Ok(value)
}
fn validate_signal(signal: &FingerprintSignalsDto) -> PortResult<()> {
    for text in [
        &signal.user_agent,
        &signal.platform,
        &signal.language,
        &signal.webgl_vendor,
        &signal.webgl_renderer,
        &signal.timezone,
    ]
    .into_iter()
    .flatten()
    {
        if text.len() > 2048 || text.chars().any(char::is_control) {
            return Err(invalid_data("invalid observation string"));
        }
    }
    if signal.memory_type.len() > 32
        || signal.memory_type.chars().any(char::is_control)
        || signal.languages.len() > 16
        || signal
            .languages
            .iter()
            .any(|s| s.len() > 128 || s.chars().any(char::is_control))
        || signal
            .pixel_ratio
            .is_some_and(|v| !v.is_finite() || v <= 0.0 || v > 16.0)
    {
        return Err(invalid_data("invalid navigator observation"));
    }
    if let Some(hints) = &signal.client_hints {
        if hints.brands.len() > 16 || hints.brands.iter().any(|s| s.len() > 256) {
            return Err(invalid_data("invalid Client Hints observation"));
        }
        for text in [
            &hints.platform,
            &hints.architecture,
            &hints.bitness,
            &hints.platform_version,
            &hints.full_version,
        ]
        .into_iter()
        .flatten()
        {
            if text.len() > 256 {
                return Err(invalid_data("Client Hints value exceeds limit"));
            }
        }
    }
    if let Some(canvas) = &signal.canvas {
        for hash in [
            &canvas.read_hash,
            &canvas.repeat_read_hash,
            &canvas.after_export_read_hash,
        ] {
            validate_hash(hash)?;
        }
        for hash in [
            canvas.export_hash.as_ref(),
            canvas.repeat_export_hash.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            validate_hash(hash)?;
        }
    }
    Ok(())
}
fn validate_hash(hash: &str) -> PortResult<()> {
    if hash.len() == 8 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(invalid_data("invalid measured hash"))
    }
}
