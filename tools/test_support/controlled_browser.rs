// Shared, loopback-only browser acceptance fixture. Compiled only by test modules.
use std::sync::Arc;

pub(crate) type CapturedProof = (String, String, serde_json::Value);

#[derive(Debug)]
pub(crate) struct ControlFixture {
    pub(crate) url: String,
    pub(crate) proofs: Arc<std::sync::Mutex<Vec<CapturedProof>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    service_version: Arc<std::sync::atomic::AtomicUsize>,
    modules: bool,
}
impl ControlFixture {
    pub(crate) fn bump_service_version(&self) {
        self.service_version
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
    pub(crate) fn wait_kind_count(
        &self,
        prefix: &str,
        kind: &str,
        count: usize,
    ) -> Vec<CapturedProof> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let proofs: Vec<_> = self
                .proofs
                .lock()
                .unwrap()
                .iter()
                .filter(|(path, _, first)| path.starts_with(prefix) && first["kind"] == kind)
                .cloned()
                .collect();
            if proofs.len() >= count || std::time::Instant::now() >= deadline {
                return proofs;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    pub(crate) fn wait_proofs(&self, prefix: &str) -> Vec<CapturedProof> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let proofs: Vec<_> = self
                .proofs
                .lock()
                .unwrap()
                .iter()
                .filter(|(path, _, _)| path.starts_with(prefix))
                .cloned()
                .collect();
            let mut expected = vec![
                "page",
                "frame",
                "cross_frame",
                "worker",
                "shared_worker",
                "service_worker",
            ];
            if self.modules {
                expected.extend([
                    "module_worker",
                    "module_shared_worker",
                    "module_service_worker",
                ]);
            }
            if expected
                .iter()
                .all(|kind| proofs.iter().any(|(_, _, value)| value["kind"] == *kind))
            {
                return proofs;
            }
            if std::time::Instant::now() >= deadline {
                return proofs;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn serve(
        mut stream: std::net::TcpStream,
        output: &std::sync::Mutex<Vec<CapturedProof>>,
        cross_origin: &str,
        service_version: &std::sync::atomic::AtomicUsize,
        modules: bool,
    ) {
        use std::io::{Read, Write};
        // Accepted sockets inherit FIONBIO on Windows. Switch only this
        // connection back to blocking I/O before applying bounded timeouts.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 2048];
        let request = loop {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break None,
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
            }
            if bytes.len() > 16384 {
                break None;
            }
            if let Some(end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&bytes[..end]).to_string();
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + length {
                    break Some((head, bytes[end + 4..end + 4 + length].to_vec()));
                }
            }
        };
        let Some((headers, body)) = request else {
            return;
        };
        let path = headers.split_whitespace().nth(1).unwrap_or("/").to_owned();
        println!("local acceptance request: {path}");
        let revision = service_version.load(std::sync::atomic::Ordering::Acquire);
        let script = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/test_support/first_observation.js"
        ))
        .replace("__BROWS_FIXTURE_VERSION__", &revision.to_string())
        .replace(
            "__BROWS_FIXTURE_PHASE__",
            if path.ends_with("dependency.mjs") {
                "module_dependency"
            } else {
                "first_script"
            },
        );
        let (content, kind) = if path.ends_with("dependency.mjs") {
            (
                format!("{script}\nexport {{first,proof}};"),
                "text/javascript",
            )
        } else if path.ends_with("shared.mjs") {
            (
                "import './dependency.mjs';onconnect=event=>event.ports[0].start();".to_owned(),
                "text/javascript",
            )
        } else if path.ends_with("service.mjs") {
            ("import {proof} from './dependency.mjs';addEventListener('install',event=>event.waitUntil(Promise.all([proof,skipWaiting()])));addEventListener('activate',event=>event.waitUntil(clients.claim()));addEventListener('message',event=>event.waitUntil(proof));".to_owned(), "text/javascript")
        } else if path.ends_with("worker.mjs") {
            (
                "import './dependency.mjs';setInterval(()=>{},1000);".to_owned(),
                "text/javascript",
            )
        } else if path.ends_with("shared.js") {
            (
                format!("{script}onconnect=event=>event.ports[0].start();"),
                "text/javascript",
            )
        } else if path.ends_with("service.js") {
            (
                    format!("{script}addEventListener('install',event=>event.waitUntil(Promise.all([proof,skipWaiting()])));addEventListener('activate',event=>event.waitUntil(clients.claim()));addEventListener('message',event=>event.waitUntil(proof));\n//service-version-{}", service_version.load(std::sync::atomic::Ordering::Acquire)),
                "text/javascript",
            )
        } else if path.ends_with("worker.js") {
            (
                format!("{script}setInterval(()=>{{}},1000);"),
                "text/javascript",
            )
        } else if path.ends_with("/version") {
            (revision.to_string(), "application/json")
        } else if path.ends_with("proof") {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) {
                println!(
                    "local first-script sample: {} CPU {} timezone {}",
                    value["kind"], value["hardwareConcurrency"], value["timezone"]
                );
                output.lock().unwrap().push((path.clone(), headers, value));
            }
            ("ok".to_owned(), "text/plain")
        } else if path.ends_with("error") {
            println!(
                "local acceptance script error: {}",
                String::from_utf8_lossy(&body)
            );
            ("ok".to_owned(), "text/plain")
        } else {
            let worker = if path.ends_with("page") {
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tools/test_support/fixture_page.js"
                ))
                .replace(
                    "__BROWS_FIXTURE_MODULES__",
                    if modules { "true" } else { "false" },
                )
            } else {
                String::new()
            };
            let frame = if path.ends_with("page") {
                format!(
                    "<iframe src='./frame'></iframe><iframe src='{cross_origin}{}frame'></iframe>",
                    path.trim_end_matches("page")
                )
            } else {
                String::new()
            };
            (
                format!(
                    "<!doctype html><meta charset='utf-8'><script>{script}{worker}</script>{frame}"
                ),
                "text/html",
            )
        };
        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nAccept-CH: Sec-CH-UA-Platform, Sec-CH-UA-Full-Version-List\r\n\r\n{content}", content.len());
        let _ = stream.write_all(response.as_bytes());
    }

    pub(crate) fn start(modules: bool) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let listener_v6 =
            std::net::TcpListener::bind(("::1", listener.local_addr().unwrap().port())).unwrap();
        let cross_origin = format!("http://localhost:{}", listener.local_addr().unwrap().port());
        listener.set_nonblocking(true).unwrap();
        listener_v6.set_nonblocking(true).unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let proofs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let halt = stop.clone();
        let output = proofs.clone();
        let service_version = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let version = service_version.clone();
        let thread = std::thread::spawn(move || {
            // Browsers preconnect before sending headers. One idle socket
            // must not block or prematurely close unrelated script loads.
            let (send, receive) = std::sync::mpsc::sync_channel::<std::net::TcpStream>(64);
            let receive = Arc::new(std::sync::Mutex::new(receive));
            std::thread::scope(|scope| {
                for _ in 0..16 {
                    let receive = receive.clone();
                    let output = output.clone();
                    let halt = halt.clone();
                    let cross_origin = &cross_origin;
                    let version = version.clone();
                    scope.spawn(move || {
                        while !halt.load(std::sync::atomic::Ordering::Acquire) {
                            let next = receive
                                .lock()
                                .unwrap()
                                .recv_timeout(std::time::Duration::from_millis(100));
                            match next {
                                Ok(stream) => {
                                    Self::serve(stream, &output, cross_origin, &version, modules)
                                }
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                                Err(_) => break,
                            }
                        }
                    });
                }
                while !halt.load(std::sync::atomic::Ordering::Acquire) {
                    let accepted = listener.accept().or_else(|error| {
                        if error.kind() == std::io::ErrorKind::WouldBlock {
                            listener_v6.accept()
                        } else {
                            Err(error)
                        }
                    });
                    match accepted {
                        Ok((stream, _)) => {
                            let _ = send.try_send(stream);
                        }
                        Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                    }
                }
                drop(send);
            });
        });
        Self {
            url,
            proofs,
            stop,
            thread: Some(thread),
            service_version,
            modules,
        }
    }
}
impl Drop for ControlFixture {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{atomic::AtomicUsize, mpsc, Mutex};
    use std::time::Duration;

    #[test]
    fn fixture_accepts_delayed_headers_on_a_nonblocking_accepted_socket() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (send, ready) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_nonblocking(true).unwrap();
            send.send(()).unwrap();
            ControlFixture::serve(
                stream,
                &Mutex::new(Vec::new()),
                "http://localhost",
                &AtomicUsize::new(0),
                false,
            );
        });
        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        client
            .write_all(b"GET /version HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        server.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.ends_with("\r\n\r\n0"));
    }
}
