#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

use brows_adapters_proxy::read_exit_ip_response;
use brows_application::{
    Clock, DiagnosticRequest, DiagnosticRunner, PortError, PortErrorKind, PortResult, ProxyRuntime,
};
use brows_domain::{
    DiagnosticCheck, DiagnosticOverall, DiagnosticReport, DiagnosticScope, DiagnosticStatus,
    ProxyLaunchBinding, SessionId,
};
use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::{Arc, Mutex},
    time::Duration,
};

pub const EXIT_IP_ENDPOINT: &str = "http://checkip.amazonaws.com/";
const EXIT_IP_HOST: &str = "checkip.amazonaws.com";
const NETWORK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub struct LocalNetworkDiagnosticRunner {
    proxies: Arc<dyn ProxyRuntime>,
    clock: Arc<dyn Clock>,
    endpoint: DiagnosticEndpoint,
    reports: Mutex<Vec<DiagnosticReport>>,
}

#[derive(Debug, Clone)]
struct DiagnosticEndpoint {
    url: String,
    host: String,
    port: u16,
}

impl LocalNetworkDiagnosticRunner {
    pub fn new(proxies: Arc<dyn ProxyRuntime>, clock: Arc<dyn Clock>) -> Self {
        Self {
            proxies,
            clock,
            endpoint: DiagnosticEndpoint {
                url: EXIT_IP_ENDPOINT.to_owned(),
                host: EXIT_IP_HOST.to_owned(),
                port: 80,
            },
            reports: Mutex::new(Vec::new()),
        }
    }

    #[cfg(test)]
    fn with_endpoint(
        proxies: Arc<dyn ProxyRuntime>,
        clock: Arc<dyn Clock>,
        host: String,
        port: u16,
    ) -> Self {
        Self {
            proxies,
            clock,
            endpoint: DiagnosticEndpoint {
                url: format!("http://{host}:{port}/"),
                host,
                port,
            },
            reports: Mutex::new(Vec::new()),
        }
    }

    pub fn reports(&self) -> PortResult<Vec<DiagnosticReport>> {
        self.reports
            .lock()
            .map(|reports| reports.clone())
            .map_err(|_| unavailable())
    }
}

impl DiagnosticRunner for LocalNetworkDiagnosticRunner {
    fn run(&self, request: &DiagnosticRequest) -> PortResult<DiagnosticReport> {
        let mut checks = static_checks(request);
        let mut endpoints = Vec::new();
        if request.include_external_checks {
            endpoints.push(self.endpoint.url.clone());
            checks.extend(self.network_checks(request));
        }
        let overall = if checks
            .iter()
            .any(|check| check.status == DiagnosticStatus::Fail)
        {
            DiagnosticOverall::Failed
        } else if checks.iter().any(|check| {
            matches!(
                check.status,
                DiagnosticStatus::Warn | DiagnosticStatus::NotChecked
            )
        }) {
            DiagnosticOverall::Attention
        } else {
            DiagnosticOverall::Healthy
        };
        let report = DiagnosticReport {
            profile_id: request.profile.id.clone(),
            session_id: request.session_id.clone(),
            generated_at_ms: self.clock.now_ms(),
            checks,
            overall,
            external_endpoints: endpoints,
        };
        self.reports
            .lock()
            .map_err(|_| unavailable())?
            .push(report.clone());
        Ok(report)
    }
}

impl LocalNetworkDiagnosticRunner {
    fn network_checks(&self, request: &DiagnosticRequest) -> Vec<DiagnosticCheck> {
        let exit = match self.fetch_exit_ip(request) {
            Ok((ip, route)) => DiagnosticCheck {
                key: "exit_ip".to_owned(),
                scope: DiagnosticScope::Configurable,
                status: DiagnosticStatus::Pass,
                observed: Some(ip),
                expected: Some(route),
                explanation:
                    "The endpoint observed this public address for the diagnostic request."
                        .to_owned(),
            },
            Err(error) => DiagnosticCheck {
                key: "exit_ip".to_owned(),
                scope: DiagnosticScope::Configurable,
                status: DiagnosticStatus::Fail,
                observed: Some("unavailable".to_owned()),
                expected: Some(
                    if request.profile.proxy_id.is_some() {
                        "configured proxy route"
                    } else {
                        "direct host route"
                    }
                    .to_owned(),
                ),
                explanation: public_network_error(error.kind).to_owned(),
            },
        };
        vec![exit,
            DiagnosticCheck { key: "dns_path".to_owned(), scope: DiagnosticScope::Configurable, status: DiagnosticStatus::NotChecked, observed: None, expected: Some("dedicated DNS test service".to_owned()), explanation: "DNS leakage is not inferred from an HTTP IP check. A dedicated correlation endpoint is required.".to_owned() },
            DiagnosticCheck { key: "webrtc_path".to_owned(), scope: DiagnosticScope::Configurable, status: DiagnosticStatus::NotChecked, observed: Some("non-proxied UDP disabled at launch".to_owned()), expected: Some("browser ICE test".to_owned()), explanation: "The launch policy is applied, but an isolated browser ICE test is required to verify the observed path.".to_owned() },
        ]
    }

    fn fetch_exit_ip(&self, request: &DiagnosticRequest) -> PortResult<(String, String)> {
        let diagnostic_session = SessionId::new(format!(
            "diagnostic-{}-{}",
            request.profile.id.as_str(),
            self.clock.now_ms()
        ))
        .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        let lease = if let Some(proxy_id) = &request.profile.proxy_id {
            let proxy = self.proxies.resolve_proxy(proxy_id)?;
            Some(self.proxies.prepare_session(&diagnostic_session, &proxy)?)
        } else {
            None
        };
        let result = (|| {
            let (host, port, proxied, route) = match lease.as_ref().map(|lease| &lease.binding) {
                Some(ProxyLaunchBinding::Loopback { host, port }) => {
                    (host.as_str(), *port, true, "configured proxy route")
                }
                Some(ProxyLaunchBinding::Direct) => {
                    return Err(PortError::new(
                        PortErrorKind::ProxyConnect,
                        "diagnostic proxy resolved to direct",
                    ))
                }
                None => (
                    self.endpoint.host.as_str(),
                    self.endpoint.port,
                    false,
                    "direct host route",
                ),
            };
            let mut stream = connect_with_timeout(host, port)?;
            stream.set_read_timeout(Some(NETWORK_TIMEOUT)).ok();
            stream.set_write_timeout(Some(NETWORK_TIMEOUT)).ok();
            if proxied {
                write!(
                    stream,
                    "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\nConnection: keep-alive\r\n\r\n",
                    self.endpoint.host, self.endpoint.port, self.endpoint.host, self.endpoint.port
                )
                .map_err(|_| {
                    PortError::new(PortErrorKind::ProxyConnect, "proxy tunnel request failed")
                })?;
                let response = read_http_head(&mut stream)?;
                if !is_success(&response) {
                    return Err(PortError::new(
                        PortErrorKind::ProxyConnect,
                        "proxy tunnel was rejected",
                    ));
                }
            }
            write!(stream, "GET / HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: Brows-Diagnostic/1\r\n\r\n", self.endpoint.host).map_err(|_| PortError::new(PortErrorKind::ProxyConnect, "exit IP request failed"))?;
            Ok((read_exit_ip_response(stream)?, route.to_owned()))
        })();
        if let Some(lease) = &lease {
            let _ = self.proxies.release_session(lease);
        }
        result
    }
}

fn connect_with_timeout(host: &str, port: u16) -> PortResult<TcpStream> {
    let addresses = (host, port).to_socket_addrs().map_err(|_| {
        PortError::new(
            PortErrorKind::ProxyConnect,
            "diagnostic destination resolution failed",
        )
    })?;
    let mut attempted = false;
    for address in addresses {
        attempted = true;
        if let Ok(stream) = TcpStream::connect_timeout(&address, NETWORK_TIMEOUT) {
            return Ok(stream);
        }
    }
    let message = if attempted {
        "diagnostic destination connection failed"
    } else {
        "diagnostic destination has no address"
    };
    Err(PortError::new(PortErrorKind::ProxyConnect, message))
}

fn read_http_head(stream: &mut TcpStream) -> PortResult<Vec<u8>> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while head.len() < 16 * 1024 {
        stream.read_exact(&mut byte).map_err(|_| {
            PortError::new(PortErrorKind::ProxyConnect, "proxy tunnel response failed")
        })?;
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            return Ok(head);
        }
    }
    Err(PortError::new(
        PortErrorKind::InvalidData,
        "proxy tunnel response was invalid",
    ))
}

fn is_success(response: &[u8]) -> bool {
    response.starts_with(b"HTTP/1.1 200") || response.starts_with(b"HTTP/1.0 200")
}

fn static_checks(request: &DiagnosticRequest) -> Vec<DiagnosticCheck> {
    let mut checks = vec![DiagnosticCheck {
        key: "profile_storage".to_owned(),
        scope: DiagnosticScope::Isolated,
        status: DiagnosticStatus::Pass,
        observed: Some("dedicated user data directory".to_owned()),
        expected: Some("isolated".to_owned()),
        explanation: "Cookies, cache and site storage are scoped to this profile.".to_owned(),
    }];
    checks.push(DiagnosticCheck {
        key: "proxy_configuration".to_owned(),
        scope: DiagnosticScope::Configurable,
        status: if request.profile.proxy_id.is_some() {
            DiagnosticStatus::Pass
        } else {
            DiagnosticStatus::Warn
        },
        observed: Some(
            if request.profile.proxy_id.is_some() {
                "proxy assigned"
            } else {
                "direct connection"
            }
            .to_owned(),
        ),
        expected: Some("explicit per-profile route".to_owned()),
        explanation: "A missing proxy means this profile shares the host network route.".to_owned(),
    });
    checks.extend(request.profile.persona.host_shared_fields().iter().map(|field| DiagnosticCheck { key: (*field).to_owned(), scope: DiagnosticScope::HostShared, status: DiagnosticStatus::Warn, observed: Some("system native".to_owned()), expected: Some("system native".to_owned()), explanation: "This version intentionally shares the host property and does not claim a separate device fingerprint.".to_owned() }));
    checks
}
fn unavailable() -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        "diagnostic report store unavailable",
    )
}
fn public_network_error(kind: PortErrorKind) -> &'static str {
    match kind {
        PortErrorKind::ProxyAuth => "Proxy authentication failed during the explicit diagnostic.",
        PortErrorKind::ProxyConnect => {
            "The configured route could not reach the fixed exit IP endpoint."
        }
        _ => "The explicit network diagnostic failed without exposing sensitive details.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brows_application::memory::{InMemoryProxyRuntime, ManualClock};
    use brows_domain::{
        BrowserBuildSelector, BrowserChannel, EnvironmentPresetRef, FingerprintPersona,
        LocaleConfig, PresetId, ProfileId, ProfileSpec, Revision, SystemNativePersona,
        WindowConfig,
    };
    use std::net::TcpListener;

    fn profile() -> ProfileSpec {
        ProfileSpec {
            id: ProfileId::new("profile-diagnostic").unwrap(),
            name: "Diagnostic".to_owned(),
            tags: Vec::new(),
            environment_preset: EnvironmentPresetRef::new(
                PresetId::new("system-native").unwrap(),
                1,
            )
            .unwrap(),
            proxy_id: None,
            browser: BrowserBuildSelector::Channel(BrowserChannel::Stable),
            persona: FingerprintPersona::SystemNative(SystemNativePersona::default()),
            locale: LocaleConfig::default(),
            window: WindowConfig::default(),
            startup_urls: Vec::new(),
            revision: Revision::FIRST,
            archived: false,
        }
    }

    #[test]
    fn local_check_never_contacts_the_disclosed_endpoint() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let runner = LocalNetworkDiagnosticRunner::with_endpoint(
            Arc::new(InMemoryProxyRuntime::default()),
            Arc::new(ManualClock::new(42)),
            "127.0.0.1".to_owned(),
            port,
        );
        let report = runner
            .run(&DiagnosticRequest {
                profile: profile(),
                session_id: None,
                include_external_checks: false,
            })
            .unwrap();
        assert!(report.external_endpoints.is_empty());
        assert!(report.checks.iter().all(|check| check.key != "exit_ip"));
        assert!(listener.accept().is_err());
    }

    #[test]
    fn explicit_direct_check_records_only_the_fixed_endpoint_and_observed_ip() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_head(&mut stream).unwrap();
            assert!(request.starts_with(b"GET / HTTP/1.1"));
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\n203.0.113.7").unwrap();
        });
        let runner = LocalNetworkDiagnosticRunner::with_endpoint(
            Arc::new(InMemoryProxyRuntime::default()),
            Arc::new(ManualClock::new(42)),
            "127.0.0.1".to_owned(),
            port,
        );
        let report = runner
            .run(&DiagnosticRequest {
                profile: profile(),
                session_id: None,
                include_external_checks: true,
            })
            .unwrap();
        server.join().unwrap();
        assert_eq!(
            report.external_endpoints,
            vec![format!("http://127.0.0.1:{port}/")]
        );
        let exit = report
            .checks
            .iter()
            .find(|check| check.key == "exit_ip")
            .unwrap();
        assert_eq!(exit.observed.as_deref(), Some("203.0.113.7"));
        assert_eq!(exit.status, DiagnosticStatus::Pass);
        assert!(report
            .checks
            .iter()
            .any(|check| check.key == "dns_path" && check.status == DiagnosticStatus::NotChecked));
        assert!(report.checks.iter().any(
            |check| check.key == "webrtc_path" && check.status == DiagnosticStatus::NotChecked
        ));
    }
}
