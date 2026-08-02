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
    net::{TcpStream, ToSocketAddrs},
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
        let result =
            test_through_broker(broker.port(), timeout).map(|exit_ip| ProxyConnectivityCheck {
                reachable: true,
                latency_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                detail: "Proxy authentication and exit request succeeded".to_owned(),
                exit_ip: Some(exit_ip),
            });
        broker.stop();
        result
    }
}

fn test_through_broker(port: u16, timeout: Duration) -> PortResult<String> {
    let address = ("127.0.0.1", port)
        .to_socket_addrs()
        .map_err(|_| PortError::new(PortErrorKind::ProxyConnect, "broker address failed"))?
        .next()
        .ok_or_else(|| PortError::new(PortErrorKind::ProxyConnect, "broker address missing"))?;
    let mut stream = TcpStream::connect_timeout(&address, timeout).map_err(|_| {
        PortError::new(
            PortErrorKind::ProxyConnect,
            "proxy broker connection failed",
        )
    })?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    write!(
        stream,
        "GET {PROXY_TEST_ENDPOINT} HTTP/1.1\r\nHost: {PROXY_TEST_HOST}\r\nConnection: close\r\nUser-Agent: Brows-Proxy-Test/1\r\n\r\n"
    )
    .map_err(|_| PortError::new(PortErrorKind::ProxyConnect, "proxy test request failed"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|_| PortError::new(PortErrorKind::ProxyConnect, "proxy test response failed"))?;
    let (head, body) = response.split_once("\r\n\r\n").ok_or_else(|| {
        PortError::new(
            PortErrorKind::InvalidData,
            "proxy test response was invalid",
        )
    })?;
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return Err(PortError::new(
            PortErrorKind::ProxyConnect,
            "proxy exit endpoint rejected the request",
        ));
    }
    let ip = body.trim();
    if ip.is_empty()
        || ip.len() > 64
        || !ip
            .chars()
            .all(|character| character.is_ascii_hexdigit() || matches!(character, '.' | ':'))
    {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "proxy exit endpoint returned an invalid address",
        ));
    }
    Ok(ip.to_owned())
}
