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
    net::{TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

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
    fn test(&self, proxy: &ProxySpec) -> PortResult<ProxyConnectivityCheck> {
        proxy
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        let timeout = Duration::from_millis(proxy.connect_timeout_ms.into());
        let addresses = (proxy.host.as_str(), proxy.port)
            .to_socket_addrs()
            .map_err(|_| {
                PortError::new(
                    PortErrorKind::ProxyConnect,
                    "proxy host could not be resolved",
                )
            })?;
        let start = Instant::now();
        for address in addresses {
            if TcpStream::connect_timeout(&address, timeout).is_ok() {
                return Ok(ProxyConnectivityCheck {
                    reachable: true,
                    latency_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                    detail: "TCP endpoint is reachable".to_owned(),
                });
            }
        }
        Err(PortError::new(
            PortErrorKind::ProxyConnect,
            "proxy TCP connection failed",
        ))
    }
}
