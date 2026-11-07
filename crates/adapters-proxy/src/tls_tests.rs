use super::*;
use brows_domain::{DnsMode, ProxyScheme};
use native_tls::{Certificate, Identity, TlsAcceptor};
use std::{
    net::{Ipv4Addr, TcpListener},
    sync::atomic::AtomicUsize,
    thread,
};

const ROOT: &[u8] = include_bytes!("../test-fixtures/root.pem");
const KEY: &[u8] = include_bytes!("../test-fixtures/server-key.pem");
const VALID: &[u8] = include_bytes!("../test-fixtures/valid.pem");
const WRONG_HOST: &[u8] = include_bytes!("../test-fixtures/wrong-host.pem");
const EXPIRED: &[u8] = include_bytes!("../test-fixtures/expired.pem");

fn trusted_fixture_connector() -> TlsConnector {
    TlsConnector::builder()
        .min_protocol_version(Some(Protocol::Tlsv12))
        .add_root_certificate(Certificate::from_pem(ROOT).unwrap())
        .build()
        .unwrap()
}

fn fixture(
    scheme: ProxyScheme,
    certificate: &'static [u8],
    response: &'static [u8],
) -> (ProxySpec, Arc<AtomicUsize>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = requests.clone();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("fixture accept failed: {e}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        match scheme {
            ProxyScheme::Http => {
                let head = read_response_head(&mut stream).unwrap();
                assert!(head.starts_with("CONNECT checkip.amazonaws.com:443 HTTP/1.1\r\n"));
                stream
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .unwrap();
            }
            ProxyScheme::Socks5 => {
                let mut greeting = [0; 3];
                stream.read_exact(&mut greeting).unwrap();
                assert_eq!(greeting, [5, 1, 0]);
                stream.write_all(&[5, 0]).unwrap();
                let mut request = [0; 5];
                stream.read_exact(&mut request).unwrap();
                assert_eq!(&request[..4], &[5, 1, 0, 3]);
                let mut destination = vec![0; request[4] as usize + 2];
                stream.read_exact(&mut destination).unwrap();
                assert_eq!(
                    &destination[..destination.len() - 2],
                    PROXY_TEST_HOST.as_bytes()
                );
                assert_eq!(
                    &destination[destination.len() - 2..],
                    &443_u16.to_be_bytes()
                );
                stream
                    .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 1, 187])
                    .unwrap();
            }
            _ => panic!("unsupported fixture protocol"),
        }
        let identity = Identity::from_pkcs8(certificate, KEY).unwrap();
        let acceptor = TlsAcceptor::new(identity).unwrap();
        if let Ok(mut tls) = acceptor.accept(stream) {
            if let Ok(request) = read_response_head(&mut tls) {
                assert!(request.starts_with("GET / HTTP/1.1\r\n"));
                assert!(request.contains("Host: checkip.amazonaws.com\r\n"));
                assert!(!request.to_ascii_lowercase().contains("proxy-authorization"));
                observed.fetch_add(1, Ordering::SeqCst);
                tls.write_all(response).unwrap();
                // Deliberately keep TLS open until the framed response has
                // been consumed. A correct client must not wait for EOF.
                let mut byte = [0];
                let _ = tls.read(&mut byte);
            }
        }
    });
    let proxy = ProxySpec {
        id: ProxyId::new("tls-fixture").unwrap(),
        name: "Offline TLS proxy".to_owned(),
        scheme,
        host: "127.0.0.1".to_owned(),
        port,
        username: None,
        secret_ref: None,
        dns_mode: DnsMode::Proxy,
        bypass_hosts: Vec::new(),
        connect_timeout_ms: 2000,
    };
    (proxy, requests, handle)
}

#[test]
fn verified_https_exit_works_through_http_and_remote_dns_socks5() {
    for scheme in [ProxyScheme::Http, ProxyScheme::Socks5] {
        let (proxy, requests, worker) = fixture(
            scheme,
            VALID,
            b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n203.0.113.9\n",
        );
        let started = Instant::now();
        let check = test_proxy_with_connector(&proxy, None, &trusted_fixture_connector()).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "must finish before the socket timeout"
        );
        assert!(check.reachable && check.exit_ip_checked);
        assert_eq!(check.exit_ip.as_deref(), Some("203.0.113.9"));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        worker.join().unwrap();
    }
}

#[test]
fn public_tester_rejects_untrusted_certificate_before_sending_http() {
    let (proxy, requests, worker) = fixture(ProxyScheme::Http, VALID, b"");
    let error = TcpProxyConnectivityTester.test(&proxy, None).unwrap_err();
    assert_eq!(error.kind, PortErrorKind::ProxyConnect);
    assert!(error.message.contains("TLS"));
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    worker.join().unwrap();
}

#[test]
fn trusted_issuer_does_not_allow_wrong_hostname_or_expired_certificate() {
    for certificate in [WRONG_HOST, EXPIRED] {
        let (proxy, requests, worker) = fixture(ProxyScheme::Http, certificate, b"");
        let error =
            test_proxy_with_connector(&proxy, None, &trusted_fixture_connector()).unwrap_err();
        assert!(error.message.contains("TLS"));
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        worker.join().unwrap();
    }
}

#[test]
fn verified_tls_does_not_hide_bad_status_or_invalid_exit_ip() {
    for response in [
        &b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n"[..],
        &b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nnot-an-ip"[..],
    ] {
        let (proxy, requests, worker) = fixture(ProxyScheme::Http, VALID, response);
        assert!(test_proxy_with_connector(&proxy, None, &trusted_fixture_connector()).is_err());
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        worker.join().unwrap();
    }
}
