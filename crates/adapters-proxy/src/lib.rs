#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

mod broker;
#[cfg(test)]
mod tls_tests;

use broker::BrokerHandle;
use brows_application::{
    PortError, PortErrorKind, PortResult, ProxyConnectivityCheck, ProxyConnectivityTester,
    ProxyLease, ProxyRepository, ProxyRuntime, SecretStore,
};
use brows_domain::{ProxyId, ProxyLaunchBinding, ProxySpec, SessionId};
use native_tls::{Protocol, TlsConnector};
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

pub const PROXY_TEST_ENDPOINT: &str = "https://checkip.amazonaws.com/";
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
        let connector = TlsConnector::builder()
            .min_protocol_version(Some(Protocol::Tlsv12))
            .build()
            .map_err(|_| {
                PortError::new(
                    PortErrorKind::Unavailable,
                    "system TLS initialization failed",
                )
            })?;
        test_proxy_with_connector(proxy, credential, &connector)
    }
}

fn test_proxy_with_connector(
    proxy: &ProxySpec,
    credential: Option<&brows_application::SecretValue>,
    connector: &TlsConnector,
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
    let result = test_https_exit_ip(broker.port(), timeout, connector).map(|exit_ip| {
        ProxyConnectivityCheck {
            reachable: true,
            latency_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            detail:
                "Proxy authentication, verified TLS certificate and HTTPS exit IP request succeeded"
                    .to_owned(),
            exit_ip: Some(exit_ip),
            exit_ip_checked: true,
        }
    });
    broker.stop();
    result
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

fn test_https_exit_ip(
    port: u16,
    timeout: Duration,
    connector: &TlsConnector,
) -> PortResult<String> {
    let mut stream = connect_to_broker(port, timeout)?;
    stream
        .write_all(b"CONNECT checkip.amazonaws.com:443 HTTP/1.1\r\nHost: checkip.amazonaws.com:443\r\n\r\n")
        .map_err(|_| PortError::new(PortErrorKind::ProxyConnect, "HTTPS tunnel test request failed"))?;
    let response = read_response_head(&mut stream)?;
    let mut status = response
        .lines()
        .next()
        .unwrap_or_default()
        .split_ascii_whitespace();
    let version = status.next();
    let code = status.next();
    if code == Some("407") {
        return Err(PortError::new(
            PortErrorKind::ProxyAuth,
            "proxy authentication failed",
        ));
    }
    if !matches!(version, Some("HTTP/1.0" | "HTTP/1.1")) || code != Some("200") {
        return Err(PortError::new(
            PortErrorKind::ProxyConnect,
            "proxy HTTPS tunnel failed",
        ));
    }
    // The system trust store and hostname checks stay enabled. CONNECT alone
    // cannot prove that the proxy delivered a trustworthy HTTPS connection.
    let mut tls = connector.connect(PROXY_TEST_HOST, stream).map_err(|_| {
        PortError::new(
            PortErrorKind::ProxyConnect,
            "proxy TLS certificate verification or handshake failed",
        )
    })?;
    write!(
        tls,
        "GET / HTTP/1.1\r\nHost: {PROXY_TEST_HOST}\r\nConnection: close\r\nUser-Agent: Brows-Proxy-Test/2\r\n\r\n"
    )
    .map_err(|_| PortError::new(PortErrorKind::ProxyConnect, "HTTPS exit IP request failed"))?;
    read_exit_ip_response(tls)
}

fn read_response_head(stream: &mut impl Read) -> PortResult<String> {
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

/// Decode a bounded IP response, finishing when HTTP framing is complete.
/// A framed response does not need a TCP EOF or a TLS close-notify to be usable.
pub fn read_exit_ip_response(mut reader: impl Read) -> PortResult<String> {
    let head = read_response_head(&mut reader)?;
    let invalid = || PortError::new(PortErrorKind::InvalidData, "exit IP response was invalid");
    let mut headers = head.trim_end_matches("\r\n").split("\r\n");
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
            let digits = value.trim();
            if length.is_some() || digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit())
            {
                return Err(invalid());
            }
            length = Some(digits.parse::<usize>().map_err(|_| invalid())?);
        }
    }
    let body = if chunked {
        if length.is_some() {
            return Err(invalid());
        }
        read_exit_chunks(&mut reader, 16 * 1024 - head.len())?
    } else if let Some(length) = length {
        if length > 64 || length > 16 * 1024 - head.len() {
            return Err(invalid());
        }
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).map_err(|_| invalid())?;
        String::from_utf8(bytes).map_err(|_| invalid())?
    } else {
        let mut bytes = Vec::new();
        reader.take(65).read_to_end(&mut bytes).map_err(|_| {
            PortError::new(PortErrorKind::ProxyConnect, "proxy test response failed")
        })?;
        if bytes.len() > 64 {
            return Err(invalid());
        }
        String::from_utf8(bytes).map_err(|_| invalid())?
    };
    let ip: IpAddr = body.trim().parse().map_err(|_| invalid())?;
    Ok(ip.to_string())
}

fn read_exit_chunks(reader: &mut impl Read, mut budget: usize) -> PortResult<String> {
    let invalid = || PortError::new(PortErrorKind::InvalidData, "exit IP chunks were invalid");
    fn line(reader: &mut impl Read, budget: &mut usize) -> PortResult<String> {
        let invalid = || PortError::new(PortErrorKind::InvalidData, "exit IP chunks were invalid");
        let mut bytes = Vec::new();
        while *budget > 0 {
            let mut byte = [0];
            reader.read_exact(&mut byte).map_err(|_| invalid())?;
            bytes.push(byte[0]);
            *budget -= 1;
            if bytes.ends_with(b"\r\n") {
                return String::from_utf8(bytes).map_err(|_| invalid());
            }
        }
        Err(invalid())
    }
    let mut body = Vec::new();
    loop {
        let size = line(reader, &mut budget)?;
        let digits = size
            .trim_end_matches("\r\n")
            .split(';')
            .next()
            .ok_or_else(invalid)?;
        if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        let count = usize::from_str_radix(digits, 16).map_err(|_| invalid())?;
        if count == 0 {
            loop {
                let trailer = line(reader, &mut budget)?;
                if trailer == "\r\n" {
                    return String::from_utf8(body).map_err(|_| invalid());
                }
                if !trailer.contains(':') {
                    return Err(invalid());
                }
            }
        }
        if count > 64 - body.len() || count + 2 > budget {
            return Err(invalid());
        }
        let mut bytes = vec![0; count + 2];
        reader.read_exact(&mut bytes).map_err(|_| invalid())?;
        budget -= bytes.len();
        if !bytes.ends_with(b"\r\n") {
            return Err(invalid());
        }
        body.extend_from_slice(&bytes[..count]);
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
            "HTTP/1.1 200 OK\r\nContent-Length: 11\r\nContent-Length: 11\r\n\r\n203.0.113.9",
            "HTTP/1.1 200 OK\r\nContent-Length: +11\r\n\r\n203.0.113.9",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n+1\r\n2\r\n0\r\n\r\n",
            "HTTP/1.1 2000 OK\r\nContent-Length: 11\r\n\r\n203.0.113.9",
        ] {
            assert!(read_exit_ip_response(response.as_bytes()).is_err());
        }
    }

    #[test]
    fn framed_exit_response_finishes_without_eof() {
        struct NoEof<'a>(&'a [u8]);
        impl Read for NoEof<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                assert!(
                    !self.0.is_empty(),
                    "must not read beyond the complete HTTP response"
                );
                self.0.read(buffer)
            }
        }
        for response in [
            "HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n203.0.113.9\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nb\r\n203.0.113.9\r\n0\r\nX-Test: bounded\r\n\r\n",
        ] {
            assert_eq!(read_exit_ip_response(NoEof(response.as_bytes())).unwrap(), "203.0.113.9");
        }
    }
}
