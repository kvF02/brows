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
            if [
                "page",
                "frame",
                "cross_frame",
                "worker",
                "shared_worker",
                "service_worker",
            ]
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
    ) {
        use std::io::{Read, Write};
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
        let capture = "({kind:typeof document==='undefined'?(typeof ServiceWorkerGlobalScope!=='undefined'&&self instanceof ServiceWorkerGlobalScope?'service_worker':typeof SharedWorkerGlobalScope!=='undefined'&&self instanceof SharedWorkerGlobalScope?'shared_worker':'worker'):window===top?'page':location.hostname==='localhost'?'cross_frame':'frame',userAgent:navigator.userAgent,platform:navigator.platform,languages:Array.from(navigator.languages),hardwareConcurrency:navigator.hardwareConcurrency,deviceMemory:navigator.deviceMemory,timezone:Intl.DateTimeFormat().resolvedOptions().timeZone,offsetWinter:new Date('2024-01-15T12:00:00Z').getTimezoneOffset(),offsetSummer:new Date('2024-07-15T12:00:00Z').getTimezoneOffset(),hints:navigator.userAgentData?.toJSON()})";
        let rendering = "(()=>{const c=typeof document==='undefined'?new OffscreenCanvas(16,16):document.createElement('canvas');c.width=16;c.height=16;const ctx=c.getContext('2d');ctx.fillStyle='#a42367';ctx.fillRect(0,0,16,16);const read=()=>Array.from(ctx.getImageData(0,0,16,16).data,b=>b.toString(16).padStart(2,'0')).join('');const canvas=read();const repeat=read();const g=new OffscreenCanvas(1,1).getContext('webgl');const ext=g?.getExtension('WEBGL_debug_renderer_info');const webgl=ext?{vendor:g.getParameter(ext.UNMASKED_VENDOR_WEBGL),renderer:g.getParameter(ext.UNMASKED_RENDERER_WEBGL)}:null;g?.getExtension('WEBGL_lose_context')?.loseContext();return{canvas,repeat,webgl};})()";
        let revision = service_version.load(std::sync::atomic::Ordering::Acquire);
        let script = format!("const first={{...{capture},fixtureVersion:{revision}}};Object.assign(first,{rendering});const send=()=>fetch('./'+first.kind+'/proof',{{method:'POST',body:JSON.stringify(first),keepalive:typeof document!=='undefined'}});const proof=send().catch(()=>send());");
        let (content, kind) = if path.ends_with("shared.js") {
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
                "addEventListener('error',event=>fetch('./error',{method:'POST',body:String(event.message)}));addEventListener('unhandledrejection',event=>fetch('./error',{method:'POST',body:String(event.reason)}));globalThis.worker=new Worker('./worker.js');globalThis.shared=new SharedWorker('./shared.js');shared.port.start();const registration=navigator.serviceWorker.register('./service.js');let revision=first.fixtureVersion;setInterval(()=>fetch('./version',{cache:'no-store'}).then(response=>response.json()).then(async next=>{if(next!==revision){revision=next;await(await registration).update();}}).catch(()=>{}),500);"
            } else {
                ""
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

    pub(crate) fn start() -> Self {
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
                                Ok(stream) => Self::serve(stream, &output, cross_origin, &version),
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
