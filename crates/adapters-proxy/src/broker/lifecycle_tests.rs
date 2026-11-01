use super::*;
use brows_domain::ProxyId;
use std::sync::mpsc;

fn proxy(scheme: ProxyScheme, port: u16) -> ProxySpec {
    ProxySpec {
        id: ProxyId::new("lifecycle-test").unwrap(),
        name: "test".into(),
        scheme,
        host: "127.0.0.1".into(),
        port,
        username: None,
        secret_ref: None,
        dns_mode: DnsMode::Proxy,
        bypass_hosts: Vec::new(),
        connect_timeout_ms: 30_000,
    }
}

fn listen() -> TcpListener {
    TcpListener::bind(("127.0.0.1", 0)).unwrap()
}

fn wait_until(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !check() {
        assert!(Instant::now() < deadline, "condition did not complete");
        thread::sleep(Duration::from_millis(5));
    }
}

fn accept(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let mut accepted = None;
    wait_until(|| match listener.accept() {
        Ok((stream, _)) => {
            accepted = Some(stream);
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
        Err(e) => panic!("accept failed: {e}"),
    });
    let stream = accepted.unwrap();
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
}

fn client(port: u16) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
}

fn stop(mut broker: BrokerHandle) -> BrokerHandle {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        broker.stop();
        tx.send(broker).unwrap();
    });
    let broker = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("stop must interrupt I/O and join workers");
    assert!(broker.thread.is_none());
    assert_eq!(broker.shutdown.tracked_count(), 0);
    assert_eq!(
        Arc::strong_count(&broker.shutdown),
        1,
        "all worker registrations were dropped"
    );
    broker
}

fn closed(mut stream: &TcpStream) {
    match stream.read(&mut [0]) {
        Ok(0) => {}
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::NotConnected
            ) => {}
        other => panic!("connection was not closed: {other:?}"),
    }
}

fn connect_tunnel(broker: &BrokerHandle, upstream: &TcpListener) -> (TcpStream, TcpStream) {
    let mut browser = client(broker.port());
    browser
        .write_all(b"CONNECT example.test:443 HTTP/1.1\r\nHost: example.test\r\n\r\n")
        .unwrap();
    let mut peer = accept(upstream);
    assert!(read_head(&mut peer)
        .unwrap()
        .starts_with(b"CONNECT example.test:443"));
    peer.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .unwrap();
    assert!(read_head(&mut browser)
        .unwrap()
        .starts_with(b"HTTP/1.1 200"));
    (browser, peer)
}

#[test]
fn stop_interrupts_an_incomplete_browser_request_and_is_idempotent() {
    let upstream = listen();
    let broker = BrokerHandle::start(
        proxy(ProxyScheme::Http, upstream.local_addr().unwrap().port()),
        None,
    )
    .unwrap();
    let port = broker.port();
    let mut browser = client(port);
    browser.write_all(b"CON").unwrap();
    wait_until(|| broker.shutdown.tracked_count() == 1);
    let mut broker = stop(broker);
    closed(&browser);
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
    broker.stop();
}

#[test]
fn stop_interrupts_http_and_socks_upstream_handshakes() {
    for scheme in [ProxyScheme::Http, ProxyScheme::Socks5] {
        let upstream = listen();
        let broker =
            BrokerHandle::start(proxy(scheme, upstream.local_addr().unwrap().port()), None)
                .unwrap();
        let mut browser = client(broker.port());
        browser
            .write_all(b"CONNECT example.test:443 HTTP/1.1\r\nHost: example.test\r\n\r\n")
            .unwrap();
        let mut peer = accept(&upstream);
        if scheme == ProxyScheme::Http {
            read_head(&mut peer).unwrap();
        } else {
            let mut greeting = [0; 3];
            peer.read_exact(&mut greeting).unwrap();
            assert_eq!(greeting, [5, 1, 0]);
        }
        // Deliberately do not answer. A 30-second handshake must be interrupted by stop.
        stop(broker);
        closed(&browser);
        closed(&peer);
    }
}

#[test]
fn stop_closes_established_tunnels_and_wakes_clients_waiting_for_capacity() {
    let upstream = listen();
    let broker = BrokerHandle::start(
        proxy(ProxyScheme::Http, upstream.local_addr().unwrap().port()),
        None,
    )
    .unwrap();
    let mut connections = Vec::new();
    for _ in 0..MAX_UPSTREAM_CONNECTIONS {
        connections.push(connect_tunnel(&broker, &upstream));
    }
    let mut queued = client(broker.port());
    queued
        .write_all(b"CONNECT example.test:443 HTTP/1.1\r\nHost: example.test\r\n\r\n")
        .unwrap();
    wait_until(|| broker.shutdown.tracked_count() == MAX_UPSTREAM_CONNECTIONS * 2 + 1);
    stop(broker);
    for (browser, peer) in connections {
        closed(&browser);
        closed(&peer);
    }
    closed(&queued);
}

#[test]
fn waiting_browser_connections_are_bounded() {
    let upstream = listen();
    let broker = BrokerHandle::start(
        proxy(ProxyScheme::Http, upstream.local_addr().unwrap().port()),
        None,
    )
    .unwrap();
    let mut browsers = Vec::new();
    for _ in 0..MAX_CLIENT_CONNECTIONS {
        let mut stream = client(broker.port());
        stream.write_all(b"CON").unwrap();
        browsers.push(stream);
    }
    wait_until(|| broker.shutdown.tracked_count() == MAX_CLIENT_CONNECTIONS);
    let excess = client(broker.port());
    closed(&excess);
    assert_eq!(broker.shutdown.tracked_count(), MAX_CLIENT_CONNECTIONS);
    stop(broker);
    for browser in browsers {
        closed(&browser);
    }
}

#[test]
fn drop_stops_tunnels_and_one_broker_does_not_cancel_another() {
    let upstream_a = listen();
    let upstream_b = listen();
    let broker_a = BrokerHandle::start(
        proxy(ProxyScheme::Http, upstream_a.local_addr().unwrap().port()),
        None,
    )
    .unwrap();
    let broker_b = BrokerHandle::start(
        proxy(ProxyScheme::Http, upstream_b.local_addr().unwrap().port()),
        None,
    )
    .unwrap();
    let (browser_a, peer_a) = connect_tunnel(&broker_a, &upstream_a);
    let (mut browser_b, mut peer_b) = connect_tunnel(&broker_b, &upstream_b);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        drop(broker_a);
        tx.send(()).unwrap();
    });
    rx.recv_timeout(Duration::from_secs(3)).unwrap();
    closed(&browser_a);
    closed(&peer_a);
    browser_b.write_all(b"request").unwrap();
    let mut bytes = [0; 7];
    peer_b.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"request");
    peer_b.write_all(b"reply").unwrap();
    let mut bytes = [0; 5];
    browser_b.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"reply");
    stop(broker_b);
}

#[test]
fn idle_tunnel_outlives_handshake_timeout_and_preserves_half_close() {
    let upstream = listen();
    let mut spec = proxy(ProxyScheme::Http, upstream.local_addr().unwrap().port());
    spec.connect_timeout_ms = 1_000;
    let broker = BrokerHandle::start(spec, None).unwrap();
    let (mut browser, mut peer) = connect_tunnel(&broker, &upstream);
    thread::sleep(Duration::from_millis(1_250));
    browser.write_all(b"body").unwrap();
    browser.shutdown(Shutdown::Write).unwrap();
    let mut body = Vec::new();
    peer.read_to_end(&mut body).unwrap();
    assert_eq!(body, b"body");
    peer.write_all(b"response").unwrap();
    peer.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    browser.read_to_end(&mut response).unwrap();
    assert_eq!(response, b"response");
    wait_until(|| broker.shutdown.tracked_count() == 0);
    stop(broker);
}

#[test]
fn cancelled_connect_never_contacts_the_upstream() {
    let upstream = listen();
    let shutdown = BrokerShutdown::default();
    shutdown.cancel();
    assert!(connect_address(
        upstream.local_addr().unwrap(),
        Duration::from_secs(30),
        &shutdown
    )
    .is_err());
    upstream.set_nonblocking(true).unwrap();
    assert_eq!(
        upstream.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn failed_upstream_does_not_fall_back_to_the_target() {
    let target = listen();
    let unused_proxy = listen();
    let proxy_port = unused_proxy.local_addr().unwrap().port();
    drop(unused_proxy);
    let broker = BrokerHandle::start(proxy(ProxyScheme::Http, proxy_port), None).unwrap();
    let mut browser = client(broker.port());
    write!(
        browser,
        "CONNECT 127.0.0.1:{} HTTP/1.1\r\nHost: target\r\n\r\n",
        target.local_addr().unwrap().port()
    )
    .unwrap();
    assert!(read_head(&mut browser)
        .unwrap()
        .starts_with(b"HTTP/1.1 502"));
    target.set_nonblocking(true).unwrap();
    assert_eq!(
        target.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    stop(broker);
}
