#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

mod broker;

use broker::BrokerHandle;
use brows_application::{
    PortError, PortErrorKind, PortResult, ProxyConnectivityCheck, ProxyConnectivityTester,
    ProxyLease, ProxyRepository, ProxyRuntime, SecretStore,
};
use brows_domain::{ProxyId, ProxyLaunchBinding, ProxySpec, SessionId};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{IpAddr, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

pub const PROXY_TEST_ENDPOINT: &str = "http://checkip.amazonaws.com/";
const PROXY_TEST_HOST: &str = "checkip.amazonaws.com";

#[derive(Debug)]
pub struct StoredProxyRuntime {
    repository: Arc<dyn ProxyRepository>,
    secrets: Arc<dyn SecretStore>,
    brokers: Mutex<BTreeMap<SessionId, (String, BrokerHandle)>>,
    sequence: AtomicU64,
}

impl StoredProxyRuntime {
    pub fn new(repository: Arc<dyn ProxyRepository>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            repository,
            secrets,
            brokers: Mutex::new(BTreeMap::new()),
            sequence: AtomicU64::new(1),
        }
    }
}

impl ProxyRuntime for StoredProxyRuntime {
    fn resolve_proxy(&self, id: &ProxyId) -> PortResult<ProxySpec> {
        self.repository.get(id)
    }

    fn prepare_session(&self, session_id: &SessionId, proxy: &ProxySpec) -> PortResult<ProxyLease> {
        if proxy.scheme == brows_domain::ProxyScheme::Https {
            return Err(PortError::new(
                PortErrorKind::CapabilityUnsupported,
                "TLS-encrypted HTTPS upstream proxies are not enabled yet",
            ));
        }
        let mut brokers = self.brokers.lock().map_err(|_| unavailable())?;
        if brokers.contains_key(session_id) {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "proxy broker already exists for session",
            ));
        }
        let secret = proxy
            .secret_ref
            .as_ref()
            .map(|reference| self.secrets.get(reference))
            .transpose()?;
        let broker = BrokerHandle::start(proxy.clone(), secret)?;
        let port = broker.port();
        let token = format!("lease-{}", self.sequence.fetch_add(1, Ordering::SeqCst));
        brokers.insert(session_id.clone(), (token.clone(), broker));
        Ok(ProxyLease {
            session_id: session_id.clone(),
            proxy_id: proxy.id.clone(),
            binding: ProxyLaunchBinding::Loopback {
                host: "127.0.0.1".to_owned(),
                port,
            },
            lease_token: token,
        })
    }

    fn release_session(&self, lease: &ProxyLease) -> PortResult<()> {
        let entry = {
            let mut brokers = self.brokers.lock().map_err(|_| unavailable())?;
            let (token, _) = brokers
                .get(&lease.session_id)
                .ok_or_else(|| PortError::not_found("proxy lease"))?;
            if token != &lease.lease_token {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    "proxy lease token does not match",
                ));
            }
            brokers.remove(&lease.session_id)
        };
        if let Some((_, mut broker)) = entry {
            broker.stop();
        }
        Ok(())
    }
}

fn unavailable() -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        "proxy broker registry is unavailable",
    )
}

#[derive(Debug, Default)]
pub struct TcpProxyConnectivityTester;
impl ProxyConnectivityTester for TcpProxyConnectivityTester {
    fn test(
        &self,
        proxy: &ProxySpec,
        credential: Option<&brows_application::SecretValue>,
    ) -> PortResult<ProxyConnectivityCheck> {
        proxy
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        if proxy.scheme == brows_domain::ProxyScheme::Https {
            return Err(PortError::new(
                PortErrorKind::CapabilityUnsupported,
                "TLS-encrypted HTTPS upstream proxies are not enabled yet",
            ));
        }
        let timeout = Duration::from_millis(proxy.connect_timeout_ms.into());
        let start = Instant::now();
        let mut broker = BrokerHandle::start(proxy.clone(), credential.cloned())?;
        let result = test_https_tunnel(broker.port(), timeout).map(|()| {
            match test_exit_ip_through_broker(broker.port(), timeout) {
                Ok(exit_ip) => ProxyConnectivityCheck {
                    reachable: true,
                    latency_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                    detail: "Proxy authentication, HTTPS tunnel and exit IP request succeeded"
                        .to_owned(),
                    exit_ip: Some(exit_ip),
                    exit_ip_checked: true,
                },
                Err(_) => ProxyConnectivityCheck {
                    reachable: true,
                    latency_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                    detail: "Proxy authentication and HTTPS tunnel succeeded; the optional HTTP exit IP endpoint was unavailable".to_owned(),
                    exit_ip: None,
                    exit_ip_checked: false,
                },
            }
        });
        broker.stop();
        result
    }
}

fn connect_to_broker(port: u16, timeout: Duration) -> PortResult<TcpStream> {
    let address = ("127.0.0.1", port)
        .to_socket_addrs()
        .map_err(|_| PortError::new(PortErrorKind::ProxyConnect, "broker address failed"))?
        .next()
        .ok_or_else(|| PortError::new(PortErrorKind::ProxyConnect, "broker address missing"))?;
    let stream = TcpStream::connect_timeout(&address, timeout).map_err(|_| {
        PortError::new(
            PortErrorKind::ProxyConnect,
            "proxy broker connection failed",
        )
    })?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    Ok(stream)
}

fn test_https_tunnel(port: u16, timeout: Duration) -> PortResult<()> {
    let mut stream = connect_to_broker(port, timeout)?;
    stream
        .write_all(b"CONNECT www.google.com:443 HTTP/1.1\r\nHost: www.google.com:443\r\nConnection: close\r\n\r\n")
        .map_err(|_| PortError::new(PortErrorKind::ProxyConnect, "HTTPS tunnel test request failed"))?;
    let response = read_response_head(&mut stream)?;
    if response.starts_with("HTTP/1.1 407") || response.starts_with("HTTP/1.0 407") {
        return Err(PortError::new(
            PortErrorKind::ProxyAuth,
            "proxy authentication failed",
        ));
    }
    if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
        return Err(PortError::new(
            PortErrorKind::ProxyConnect,
            "proxy HTTPS tunnel failed",
        ));
    }
    Ok(())
}

fn read_response_head(stream: &mut TcpStream) -> PortResult<String> {
    let mut bytes = Vec::new();
    let mut byte = [0_u8; 1];
    while bytes.len() < 16 * 1024 {
        stream.read_exact(&mut byte).map_err(|_| {
            PortError::new(PortErrorKind::ProxyConnect, "proxy response header failed")
        })?;
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            return String::from_utf8(bytes).map_err(|_| {
                PortError::new(
                    PortErrorKind::InvalidData,
                    "proxy response header was invalid",
                )
            });
        }
    }
    Err(PortError::new(
        PortErrorKind::InvalidData,
        "proxy response header was too large",
    ))
}

fn test_exit_ip_through_broker(port: u16, timeout: Duration) -> PortResult<String> {
    let mut stream = connect_to_broker(port, timeout)?;
    write!(
        stream,
        "GET {PROXY_TEST_ENDPOINT} HTTP/1.1\r\nHost: {PROXY_TEST_HOST}\r\nConnection: close\r\nUser-Agent: Brows-Proxy-Test/1\r\n\r\n"
    )
    .map_err(|_| PortError::new(PortErrorKind::ProxyConnect, "proxy test request failed"))?;
    read_exit_ip_response(stream)
}

/// Decode the small plaintext IP response used by the explicit exit checks.
pub fn read_exit_ip_response(reader: impl Read) -> PortResult<String> {
    const MAX_RESPONSE: u64 = 16 * 1024;
    let mut response = String::new();
    reader
        .take(MAX_RESPONSE + 1)
        .read_to_string(&mut response)
        .map_err(|_| PortError::new(PortErrorKind::ProxyConnect, "proxy test response failed"))?;
    let invalid = || PortError::new(PortErrorKind::InvalidData, "exit IP response was invalid");
    if response.len() as u64 > MAX_RESPONSE {
        return Err(invalid());
    }
    let (head, body) = response.split_once("\r\n\r\n").ok_or_else(invalid)?;
    let mut headers = head.split("\r\n");
    let mut status = headers.next().ok_or_else(invalid)?.split_ascii_whitespace();
    match (status.next(), status.next()) {
        (Some("HTTP/1.0" | "HTTP/1.1"), Some("200")) => {}
        (Some("HTTP/1.0" | "HTTP/1.1"), Some("407")) => {
            return Err(PortError::new(
                PortErrorKind::ProxyAuth,
                "proxy authentication failed",
            ))
        }
        _ => {
            return Err(PortError::new(
                PortErrorKind::ProxyConnect,
                "proxy exit endpoint rejected the request",
            ))
        }
    }
    let mut chunked = false;
    let mut length = None;
    for header in headers {
        let (name, value) = header.split_once(':').ok_or_else(invalid)?;
        if name.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.trim().eq_ignore_ascii_case("chunked") {
                return Err(invalid());
            }
            chunked = true;
        } else if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err(invalid());
            }
            length = Some(value.trim().parse::<usize>().map_err(|_| invalid())?);
        }
    }
    let decoded;
    let text = if chunked {
        if length.is_some() {
            return Err(invalid());
        }
        decoded = decode_exit_chunks(body).ok_or_else(invalid)?;
        decoded.as_str()
    } else {
        if length.is_some_and(|expected| expected != body.len()) {
            return Err(invalid());
        }
        body
    };
    let ip: IpAddr = text.trim().parse().map_err(|_| invalid())?;
    Ok(ip.to_string())
}

fn decode_exit_chunks(mut encoded: &str) -> Option<String> {
    let mut result = String::new();
    loop {
        let (size, data) = encoded.split_once("\r\n")?;
        let digits = size.split(';').next()?;
        let count = usize::from_str_radix(digits, 16).ok()?;
        if count == 0 {
            let valid_end = data == "\r\n"
                || data.strip_suffix("\r\n\r\n").is_some_and(|trailers| {
                    trailers.split("\r\n").all(|header| header.contains(':'))
                });
            return valid_end.then_some(result);
        }
        if count > 64 - result.len() {
            return None;
        }
        result.push_str(data.get(..count)?);
        encoded = data.get(count..)?.strip_prefix("\r\n")?;
    }
}

#[cfg(test)]
mod exit_response_tests {
    use super::*;

    #[test]
    fn exit_response_accepts_chunked_fixed_length_and_close_delimited_ip() {
        for (response, ip) in [
            ("HTTP/1.1 200 OK\r\nTransfer-Encoding: Chunked\r\n\r\n7\r\n203.0.1\r\n5\r\n13.9\n\r\n0\r\n\r\n", "203.0.113.9"),
            ("HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n203.0.113.9\n", "203.0.113.9"),
            ("HTTP/1.0 200 OK\r\n\r\n2001:db8::1\n", "2001:db8::1"),
        ] {
            assert_eq!(read_exit_ip_response(response.as_bytes()).unwrap(), ip);
        }
    }

    #[test]
    fn exit_response_rejects_bad_ip_and_incomplete_or_ambiguous_framing() {
        for response in [
            "HTTP/1.1 200 OK\r\n\r\n999.0.113.9",
            "HTTP/1.1 200 OK\r\nContent-Length: 99\r\n\r\n203.0.113.9",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nb\r\n203.0.113.9\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 11\r\n\r\nb\r\n203.0.113.9\r\n0\r\n\r\n",
        ] {
            assert!(read_exit_ip_response(response.as_bytes()).is_err());
        }
    }
}
