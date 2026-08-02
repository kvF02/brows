use base64::{engine::general_purpose::STANDARD, Engine};
use brows_application::{PortError, PortErrorKind, PortResult, SecretValue};
use brows_domain::{ProxyScheme, ProxySpec};
use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

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
        let thread = thread::Builder::new()
            .name(format!("brows-proxy-{port}"))
            .spawn(move || {
                while !flag.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((client, _)) => {
                            let p = proxy.clone();
                            let s = secret.clone();
                            let _ = thread::Builder::new()
                                .name("brows-proxy-connection".into())
                                .spawn(move || {
                                    let _ = handle(client, &p, s.as_deref());
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
        client
            .write_all(b"HTTP/1.1 501 Not Implemented\r\nConnection: close\r\n\r\n")
            .ok();
        return Err(PortError::new(
            PortErrorKind::CapabilityUnsupported,
            "SOCKS5 plain HTTP forwarding is not enabled",
        ));
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
    let mut s = TcpStream::connect((proxy.host.as_str(), proxy.port))
        .map_err(|e| ioerr("connect upstream proxy", e))?;
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
    Ok(s)
}
fn socks_connect(
    proxy: &ProxySpec,
    secret: Option<&SecretValue>,
    target: &str,
) -> PortResult<TcpStream> {
    let mut s = TcpStream::connect((proxy.host.as_str(), proxy.port))
        .map_err(|e| ioerr("connect SOCKS5 proxy", e))?;
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
    Ok(s)
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
    use brows_domain::{DnsMode, ProxyId};
    use std::sync::mpsc;

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
        let proxy = ProxySpec {
            id: ProxyId::new("proxy-test").unwrap(),
            name: "test".to_owned(),
            scheme: ProxyScheme::Http,
            host: "127.0.0.1".to_owned(),
            port,
            username: None,
            secret_ref: None,
            dns_mode: DnsMode::Proxy,
            bypass_hosts: vec![],
            connect_timeout_ms: 2_000,
        };
        let mut broker = BrokerHandle::start(proxy, None).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", broker.port())).unwrap();
        client
            .write_all(b"CONNECT example.test:443 HTTP/1.1\r\nHost: example.test\r\n\r\n")
            .unwrap();
        let response = read_head(&mut client).unwrap();
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));
        rx.recv().unwrap();
        broker.stop();
    }
}
