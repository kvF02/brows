#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

use brows_application::{
    PortError, PortErrorKind, PortResult, ProxyConnectivityCheck, ProxyConnectivityTester,
    ProxyLease, ProxyRepository, ProxyRuntime,
};
use brows_domain::{ProxyId, ProxySpec, SessionId};
use std::{
    net::{TcpStream, ToSocketAddrs},
    sync::Arc,
    time::{Duration, Instant},
};

/// Persistent control-plane runtime. Session forwarding remains fail-closed
/// until the loopback broker adapter is enabled in the next vertical slice.
#[derive(Debug)]
pub struct StoredProxyRuntime {
    repository: Arc<dyn ProxyRepository>,
}

impl StoredProxyRuntime {
    pub fn new(repository: Arc<dyn ProxyRepository>) -> Self {
        Self { repository }
    }
}

impl ProxyRuntime for StoredProxyRuntime {
    fn resolve_proxy(&self, id: &ProxyId) -> PortResult<ProxySpec> {
        self.repository.get(id)
    }

    fn prepare_session(
        &self,
        _session_id: &SessionId,
        _proxy: &ProxySpec,
    ) -> PortResult<ProxyLease> {
        Err(PortError::new(
            PortErrorKind::CapabilityUnsupported,
            "proxy is saved, but the fail-closed session broker is not enabled yet",
        ))
    }

    fn release_session(&self, _lease: &ProxyLease) -> PortResult<()> {
        Ok(())
    }
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
        let mut attempted = false;
        for address in addresses {
            attempted = true;
            if TcpStream::connect_timeout(&address, timeout).is_ok() {
                return Ok(ProxyConnectivityCheck {
                    reachable: true,
                    latency_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                    detail: "TCP endpoint is reachable; protocol authentication is not tested yet"
                        .to_owned(),
                });
            }
        }
        if !attempted {
            return Err(PortError::new(
                PortErrorKind::ProxyConnect,
                "proxy host returned no usable address",
            ));
        }
        Err(PortError::new(
            PortErrorKind::ProxyConnect,
            "proxy TCP connection failed",
        ))
    }
}
