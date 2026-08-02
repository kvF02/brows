use base64::{engine::general_purpose::STANDARD, Engine};
use brows_application::{PortError, PortErrorKind, PortResult, SecretValue};
use brows_domain::{ProxyScheme, ProxySpec};
use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const MAX_UPSTREAM_CONNECTIONS: usize = 6;

pub(crate) struct BrokerHandle {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl std::fmt::Debug for BrokerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrokerHandle")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl BrokerHandle {
    pub fn start(proxy: ProxySpec, secret: Option<SecretValue>) -> PortResult<Self> {
        let listener =
            TcpListener::bind(("127.0.0.1", 0)).map_err(|e| ioerr("bind loopback broker", e))?;
        let port = listener
            .local_addr()
            .map_err(|e| ioerr("read broker address", e))?
            .port();
        listener
            .set_nonblocking(true)
            .map_err(|e| ioerr("configure broker listener", e))?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let secret = secret.map(Arc::new);
        let gate = Arc::new(ConnectionGate::new(MAX_UPSTREAM_CONNECTIONS));
        let thread = thread::Builder::new()
            .name(format!("brows-proxy-{port}"))
            .spawn(move || {
                while !flag.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((client, _)) => {
                            let p = proxy.clone();
                            let s = secret.clone();
                            let g = gate.clone();
                            let _ = thread::Builder::new()
                                .name("brows-proxy-connection".into())
                                .spawn(move || {
                                    handle_with_failure_response(client, &p, s.as_deref(), &g);
                                });
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(20))
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| ioerr("start broker thread", e))?;
        Ok(Self {
            port,
            stop,
            thread: Some(thread),
        })
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn handle_with_failure_response(
    client: TcpStream,
    proxy: &ProxySpec,
    secret: Option<&SecretValue>,
    gate: &ConnectionGate,
) {
    let fallback = client.try_clone().ok();
    let timeout = Duration::from_millis(proxy.connect_timeout_ms.into());
    let result = gate
        .acquire(timeout)
        .and_then(|_permit| handle(client, proxy, secret));
    if let Err(error) = result {
        if let Some(mut client) = fallback {
            let (status, reason) = if error.kind == PortErrorKind::ProxyAuth {
                (
                    "407 Proxy Authentication Required",
                    "proxy authentication failed",
                )
            } else {
                ("502 Bad Gateway", "proxy route failed")
            };
            let body = reason.as_bytes();
            let _ = write!(
                client,
                "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reason}",
                body.len()
            );
            let _ = client.shutdown(Shutdown::Both);
        }
    }
}

#[derive(Debug)]
struct ConnectionGate {
    available: Mutex<usize>,
    changed: Condvar,
    capacity: usize,
}

impl ConnectionGate {
    fn new(capacity: usize) -> Self {
        Self {
            available: Mutex::new(capacity),
            changed: Condvar::new(),
            capacity,
        }
    }

    fn acquire(&self, timeout: Duration) -> PortResult<ConnectionPermit<'_>> {
        let available = self.available.lock().map_err(|_| {
            PortError::new(
                PortErrorKind::Unavailable,
                "proxy connection gate unavailable",
            )
        })?;
        let (mut available, wait) = self
            .changed
            .wait_timeout_while(available, timeout, |count| *count == 0)
            .map_err(|_| {
                PortError::new(
                    PortErrorKind::Unavailable,
                    "proxy connection gate unavailable",
                )
            })?;
        if wait.timed_out() && *available == 0 {
            return Err(PortError::new(
                PortErrorKind::ProxyConnect,
                "upstream proxy connection limit remained busy",
            ));
        }
        *available -= 1;
        Ok(ConnectionPermit { gate: self })
    }

    fn release(&self) {
        if let Ok(mut available) = self.available.lock() {
            *available = (*available + 1).min(self.capacity);
            self.changed.notify_one();
        }
    }
}

#[derive(Debug)]
struct ConnectionPermit<'a> {
    gate: &'a ConnectionGate,
}

impl Drop for ConnectionPermit<'_> {
    fn drop(&mut self) {
        self.gate.release();
    }
}
impl Drop for BrokerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn handle(
    mut client: TcpStream,
    proxy: &ProxySpec,
    secret: Option<&SecretValue>,
) -> PortResult<()> {
    client
        .set_nonblocking(false)
        .map_err(|e| ioerr("configure proxy client socket", e))?;
    client
        .set_read_timeout(Some(Duration::from_millis(proxy.connect_timeout_ms.into())))
        .ok();
    let head = read_head(&mut client)?;
    let line = head.split(|b| *b == b'\n').next().unwrap_or(&[]);
    let line = String::from_utf8_lossy(line);
    let mut parts = line.trim().split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    if method.eq_ignore_ascii_case("CONNECT") {
        let upstream = connect_target(proxy, secret, target)?;
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .map_err(|e| ioerr("acknowledge tunnel", e))?;
        tunnel(client, upstream);
        return Ok(());
    }
    if matches!(proxy.scheme, ProxyScheme::Socks5) {
        let destination = parse_absolute_http_target(target)?;
        let mut upstream = socks_connect(proxy, secret, &destination.authority)?;
        let forwarded = rewrite_request_target(&head, &destination.path_and_query)?;
        upstream
            .write_all(&forwarded)
            .map_err(|e| ioerr("forward HTTP request through SOCKS5", e))?;
        tunnel(client, upstream);
        return Ok(());
    }
    let mut upstream = TcpStream::connect((proxy.host.as_str(), proxy.port))
        .map_err(|e| ioerr("connect upstream proxy", e))?;
    let forwarded = inject_auth(&head, proxy, secret)?;
    upstream
        .write_all(&forwarded)
        .map_err(|e| ioerr("forward HTTP request", e))?;
    tunnel(client, upstream);
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct HttpDestination {
    authority: String,
    path_and_query: String,
}

fn parse_absolute_http_target(target: &str) -> PortResult<HttpDestination> {
    let remainder = target.strip_prefix("http://").ok_or_else(|| {
        PortError::new(
            PortErrorKind::InvalidData,
            "plain proxy request must use an absolute HTTP target",
        )
    })?;
    let boundary = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..boundary];
    if authority.is_empty() || authority.contains('@') || authority.contains('#') {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "plain proxy request has an invalid authority",
        ));
    }
    let authority = normalize_http_authority(authority)?;
    let suffix = &remainder[boundary..];
    if suffix.starts_with('#') || suffix.contains('#') {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "plain proxy request must not contain a fragment",
        ));
    }
    let path_and_query = if suffix.is_empty() {
        "/".to_owned()
    } else if suffix.starts_with('?') {
        format!("/{suffix}")
    } else {
        suffix.to_owned()
    };
    Ok(HttpDestination {
        authority,
        path_and_query,
    })
}

fn normalize_http_authority(authority: &str) -> PortResult<String> {
    if authority.starts_with('[') {
        let close = authority.find(']').ok_or_else(|| {
            PortError::new(PortErrorKind::InvalidData, "invalid IPv6 HTTP authority")
        })?;
        let tail = &authority[close + 1..];
        return match tail {
            "" => Ok(format!("{authority}:80")),
            value if value.starts_with(':') && value[1..].parse::<u16>().is_ok() => {
                Ok(authority.to_owned())
            }
            _ => Err(PortError::new(
                PortErrorKind::InvalidData,
                "invalid IPv6 HTTP port",
            )),
        };
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.parse::<u16>().is_ok() => {
            Ok(authority.to_owned())
        }
        Some(_) => Err(PortError::new(
            PortErrorKind::InvalidData,
            "invalid HTTP authority port",
        )),
        None => Ok(format!("{authority}:80")),
    }
}

fn rewrite_request_target(head: &[u8], target: &str) -> PortResult<Vec<u8>> {
    let line_end = head
        .windows(2)
        .position(|window| window == b"\r\n")
        .ok_or_else(|| {
            PortError::new(
                PortErrorKind::InvalidData,
                "HTTP request line is incomplete",
            )
        })?;
    let line = std::str::from_utf8(&head[..line_end]).map_err(|_| {
        PortError::new(PortErrorKind::InvalidData, "HTTP request line is not UTF-8")
    })?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let _old_target = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();
    if method.is_empty() || version != "HTTP/1.1" && version != "HTTP/1.0" || parts.next().is_some()
    {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "HTTP request line is invalid",
        ));
    }
    let mut rewritten = format!("{method} {target} {version}\r\n").into_bytes();
    rewritten.extend_from_slice(&head[line_end + 2..]);
    Ok(rewritten)
}

fn connect_target(
    proxy: &ProxySpec,
    secret: Option<&SecretValue>,
    target: &str,
) -> PortResult<TcpStream> {
    match proxy.scheme {
        ProxyScheme::Http | ProxyScheme::Https => http_connect(proxy, secret, target),
        ProxyScheme::Socks5 => socks_connect(proxy, secret, target),
    }
}
fn http_connect(
    proxy: &ProxySpec,
    secret: Option<&SecretValue>,
    target: &str,
) -> PortResult<TcpStream> {
    let mut s = connect_upstream(proxy, "connect upstream proxy")?;
    let auth = auth_header(proxy, secret)?;
    write!(
        s,
        "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n{auth}Connection: keep-alive\r\n\r\n"
    )
    .map_err(|e| ioerr("send upstream CONNECT", e))?;
    let response = read_head(&mut s)?;
    if !response.starts_with(b"HTTP/1.1 200") && !response.starts_with(b"HTTP/1.0 200") {
        return Err(PortError::new(
            PortErrorKind::ProxyAuth,
            "upstream proxy rejected CONNECT",
        ));
    }
    clear_transport_timeouts(&s);
    Ok(s)
}
fn socks_connect(
    proxy: &ProxySpec,
    secret: Option<&SecretValue>,
    target: &str,
) -> PortResult<TcpStream> {
    let mut s = connect_upstream(proxy, "connect SOCKS5 proxy")?;
    let auth = proxy.username.is_some();
    s.write_all(if auth { &[5, 1, 2] } else { &[5, 1, 0] })
        .map_err(|e| ioerr("send SOCKS greeting", e))?;
    let mut r = [0; 2];
    s.read_exact(&mut r)
        .map_err(|e| ioerr("read SOCKS greeting", e))?;
    if auth {
        if r[1] != 2 {
            return Err(PortError::new(
                PortErrorKind::ProxyAuth,
                "SOCKS5 authentication unavailable",
            ));
        }
        let u = proxy.username.as_deref().unwrap_or("").as_bytes();
        let p = secret
            .ok_or_else(|| PortError::new(PortErrorKind::ProxyAuth, "proxy credential missing"))?
            .expose();
        if u.len() > 255 || p.len() > 255 {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "SOCKS5 credential too long",
            ));
        }
        let mut b = vec![1, u.len() as u8];
        b.extend(u);
        b.push(p.len() as u8);
        b.extend(p);
        s.write_all(&b).map_err(|e| ioerr("send SOCKS auth", e))?;
        s.read_exact(&mut r)
            .map_err(|e| ioerr("read SOCKS auth", e))?;
        if r[1] != 0 {
            return Err(PortError::new(
                PortErrorKind::ProxyAuth,
                "SOCKS5 authentication failed",
            ));
        }
    } else if r[1] != 0 {
        return Err(PortError::new(
            PortErrorKind::ProxyAuth,
            "SOCKS5 no-auth unavailable",
        ));
    }
    let (host, port) = split_target(target)?;
    if host.len() > 255 {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "target host too long",
        ));
    }
    let mut b = vec![5, 1, 0, 3, host.len() as u8];
    b.extend(host.as_bytes());
    b.extend(port.to_be_bytes());
    s.write_all(&b)
        .map_err(|e| ioerr("send SOCKS connect", e))?;
    let mut h = [0; 4];
    s.read_exact(&mut h)
        .map_err(|e| ioerr("read SOCKS connect", e))?;
    if h[1] != 0 {
        return Err(PortError::new(
            PortErrorKind::ProxyConnect,
            "SOCKS5 connect rejected",
        ));
    }
    let n = match h[3] {
        1 => 4,
        4 => 16,
        3 => {
            let mut x = [0];
            s.read_exact(&mut x)
                .map_err(|e| ioerr("read SOCKS address", e))?;
            x[0] as usize
        }
        _ => {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "invalid SOCKS response",
            ))
        }
    };
    let mut tail = vec![0; n + 2];
    s.read_exact(&mut tail)
        .map_err(|e| ioerr("read SOCKS address", e))?;
    clear_transport_timeouts(&s);
    Ok(s)
}

fn clear_transport_timeouts(stream: &TcpStream) {
    stream.set_read_timeout(None).ok();
    stream.set_write_timeout(None).ok();
}

fn connect_upstream(proxy: &ProxySpec, context: &'static str) -> PortResult<TcpStream> {
    let timeout = Duration::from_millis(proxy.connect_timeout_ms.into());
    let addresses = (proxy.host.as_str(), proxy.port)
        .to_socket_addrs()
        .map_err(|_| PortError::new(PortErrorKind::ProxyConnect, "proxy host resolution failed"))?;
    for address in addresses {
        if let Ok(stream) = TcpStream::connect_timeout(&address, timeout) {
            stream.set_read_timeout(Some(timeout)).ok();
            stream.set_write_timeout(Some(timeout)).ok();
            return Ok(stream);
        }
    }
    Err(PortError::new(
        PortErrorKind::ProxyConnect,
        format!("{context}: connection timed out or failed"),
    ))
}
fn split_target(v: &str) -> PortResult<(&str, u16)> {
    let (h, p) = v
        .rsplit_once(':')
        .ok_or_else(|| PortError::new(PortErrorKind::InvalidData, "CONNECT target has no port"))?;
    Ok((
        h,
        p.parse()
            .map_err(|_| PortError::new(PortErrorKind::InvalidData, "invalid target port"))?,
    ))
}
fn auth_header(proxy: &ProxySpec, secret: Option<&SecretValue>) -> PortResult<String> {
    if let Some(u) = &proxy.username {
        let p = secret
            .ok_or_else(|| PortError::new(PortErrorKind::ProxyAuth, "proxy credential missing"))?;
        let mut raw = u.as_bytes().to_vec();
        raw.push(b':');
        raw.extend(p.expose());
        Ok(format!(
            "Proxy-Authorization: Basic {}\r\n",
            STANDARD.encode(raw)
        ))
    } else {
        Ok(String::new())
    }
}
fn inject_auth(
    head: &[u8],
    proxy: &ProxySpec,
    secret: Option<&SecretValue>,
) -> PortResult<Vec<u8>> {
    let auth = auth_header(proxy, secret)?;
    if auth.is_empty() {
        return Ok(head.to_vec());
    }
    let mut out = head[..head.len() - 2].to_vec();
    out.extend(auth.as_bytes());
    out.extend(b"\r\n");
    Ok(out)
}
fn read_head(s: &mut TcpStream) -> PortResult<Vec<u8>> {
    let mut out = Vec::new();
    let mut b = [0; 1024];
    while out.len() < 65536 {
        let n = s.read(&mut b).map_err(|e| ioerr("read proxy header", e))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&b[..n]);
        if out.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(out);
        }
    }
    Err(PortError::new(
        PortErrorKind::InvalidData,
        "proxy header incomplete or too large",
    ))
}
fn tunnel(a: TcpStream, b: TcpStream) {
    let (mut ar, mut aw) = (a.try_clone().ok(), a);
    let (mut br, mut bw) = (b.try_clone().ok(), b);
    if let (Some(mut ar), Some(mut br)) = (ar.take(), br.take()) {
        let t = thread::spawn(move || {
            let _ = std::io::copy(&mut ar, &mut bw);
            let _ = bw.shutdown(Shutdown::Write);
        });
        let _ = std::io::copy(&mut br, &mut aw);
        let _ = aw.shutdown(Shutdown::Write);
        let _ = t.join();
    }
}
fn ioerr(context: &'static str, e: std::io::Error) -> PortError {
    PortError::new(PortErrorKind::ProxyConnect, format!("{context}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use brows_domain::{DnsMode, ProxyId, SecretRef};
    use std::sync::mpsc;

    fn proxy(scheme: ProxyScheme, port: u16, authenticated: bool) -> ProxySpec {
        ProxySpec {
            id: ProxyId::new("proxy-test").unwrap(),
            name: "test".to_owned(),
            scheme,
            host: "127.0.0.1".to_owned(),
            port,
            username: authenticated.then(|| "alice".to_owned()),
            secret_ref: authenticated.then(|| SecretRef::new("secret-test").unwrap()),
            dns_mode: DnsMode::Proxy,
            bypass_hosts: vec![],
            connect_timeout_ms: 2_000,
        }
    }

    #[test]
    fn http_connect_broker_requires_upstream_success() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = upstream.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = upstream.accept().unwrap();
            let request = read_head(&mut stream).unwrap();
            assert!(String::from_utf8_lossy(&request).starts_with("CONNECT example.test:443"));
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .unwrap();
            tx.send(()).unwrap();
            let mut byte = [0; 1];
            let _ = stream.read(&mut byte);
        });
        let mut broker = BrokerHandle::start(proxy(ProxyScheme::Http, port, false), None).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", broker.port())).unwrap();
        client
            .write_all(b"CONNECT example.test:443 HTTP/1.1\r\nHost: example.test\r\n\r\n")
            .unwrap();
        let response = read_head(&mut client).unwrap();
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));
        rx.recv().unwrap();
        broker.stop();
    }

    #[test]
    fn authenticated_socks5_forwards_plain_http_with_remote_dns() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = upstream.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = upstream.accept().unwrap();
            let mut greeting = [0_u8; 3];
            stream.read_exact(&mut greeting).unwrap();
            assert_eq!(greeting, [5, 1, 2]);
            stream.write_all(&[5, 2]).unwrap();

            let mut auth_head = [0_u8; 2];
            stream.read_exact(&mut auth_head).unwrap();
            assert_eq!(auth_head, [1, 5]);
            let mut username = vec![0_u8; auth_head[1] as usize];
            stream.read_exact(&mut username).unwrap();
            let mut password_length = [0_u8; 1];
            stream.read_exact(&mut password_length).unwrap();
            let mut password = vec![0_u8; password_length[0] as usize];
            stream.read_exact(&mut password).unwrap();
            assert_eq!(username, b"alice");
            assert_eq!(password, b"correct horse");
            stream.write_all(&[1, 0]).unwrap();

            let mut connect_head = [0_u8; 5];
            stream.read_exact(&mut connect_head).unwrap();
            assert_eq!(&connect_head[..4], &[5, 1, 0, 3]);
            let mut host = vec![0_u8; connect_head[4] as usize];
            stream.read_exact(&mut host).unwrap();
            let mut target_port = [0_u8; 2];
            stream.read_exact(&mut target_port).unwrap();
            assert_eq!(host, b"example.test");
            assert_eq!(u16::from_be_bytes(target_port), 80);
            stream
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80])
                .unwrap();

            let request = read_head(&mut stream).unwrap();
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("GET /path?q=1 HTTP/1.1\r\n"));
            assert!(request.contains("Host: example.test\r\n"));
            assert!(!request.contains("http://example.test"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
                .unwrap();
            tx.send(()).unwrap();
        });

        let secret = SecretValue::new(b"correct horse".to_vec()).unwrap();
        let mut broker =
            BrokerHandle::start(proxy(ProxyScheme::Socks5, port, true), Some(secret)).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", broker.port())).unwrap();
        client
            .write_all(
                b"GET http://example.test/path?q=1 HTTP/1.1\r\nHost: example.test\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.ends_with("OK"));
        rx.recv().unwrap();
        broker.stop();
    }

    #[test]
    fn absolute_http_target_is_strict_and_rewritten_to_origin_form() {
        assert_eq!(
            parse_absolute_http_target("http://example.test?q=1").unwrap(),
            HttpDestination {
                authority: "example.test:80".to_owned(),
                path_and_query: "/?q=1".to_owned(),
            }
        );
        assert!(parse_absolute_http_target("https://example.test/").is_err());
        assert!(parse_absolute_http_target("http://user@example.test/").is_err());
        assert!(parse_absolute_http_target("http://example.test/#fragment").is_err());

        let rewritten = rewrite_request_target(
            b"GET http://example.test/a HTTP/1.1\r\nHost: example.test\r\n\r\n",
            "/a",
        )
        .unwrap();
        assert!(rewritten.starts_with(b"GET /a HTTP/1.1\r\n"));
    }

    #[test]
    fn established_tunnel_does_not_inherit_handshake_timeouts() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = std::thread::spawn(move || listener.accept().unwrap().0);
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let peer = accept.join().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        clear_transport_timeouts(&stream);
        assert_eq!(stream.read_timeout().unwrap(), None);
        assert_eq!(stream.write_timeout().unwrap(), None);
        drop(peer);
    }

    #[test]
    fn accepted_client_socket_is_restored_to_blocking_mode() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let accepted = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::yield_now()
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        };
        accepted.set_nonblocking(false).unwrap();
        accepted
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        assert!(accepted.read_timeout().unwrap().is_some());
        drop((client, accepted));
    }
}
