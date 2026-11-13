//! Bounded local observation of the production MAIN-world hook template.
//! No debug port, sandbox bypass, persisted user profile or external website.

use super::{internal, invalid_data, is_reparse_point, now_ms};
use brows_application::{PortError, PortErrorKind, PortResult};
use brows_domain::{BrowserBuild, BrowserBuildId, FingerprintSupport, ProfileId};
use serde_json::Value;
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MAX_DUMP_BYTES: usize = 64 * 1024;
const MAX_REQUEST_BYTES: usize = 4096;
const PROCESS_TIMEOUT: Duration = Duration::from_secs(25);
const MARKER_FILE: &str = ".brows-fingerprint-probe-owner";
const PRE_START: &str = "<pre id=\"brows-fingerprint-probe\">";
static NEXT_PROBE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub(super) struct ProbeSlot<'a> {
    slots: &'a Mutex<HashSet<BrowserBuildId>>,
    build_id: BrowserBuildId,
}

impl<'a> ProbeSlot<'a> {
    pub(super) fn acquire(
        slots: &'a Mutex<HashSet<BrowserBuildId>>,
        build_id: &BrowserBuildId,
    ) -> PortResult<Self> {
        let mut active = slots
            .lock()
            .map_err(|_| internal("fingerprint probe lock is poisoned"))?;
        if !active.insert(build_id.clone()) {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "此运行时已有指纹能力检测正在执行",
            ));
        }
        Ok(Self {
            slots,
            build_id: build_id.clone(),
        })
    }
}

impl Drop for ProbeSlot<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.slots.lock() {
            active.remove(&self.build_id);
        }
    }
}

#[derive(Debug)]
pub(super) struct Observation {
    pub support: FingerprintSupport,
    pub detail: String,
}

pub(super) fn run(build: &BrowserBuild, profile_root: &Path) -> PortResult<Observation> {
    #[cfg(windows)]
    if build.capabilities.supports_devtools_pipe || build.id.as_str().starts_with("managed-") {
        return run_private(build, profile_root);
    }
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    let directory = ProbeDirectory::create(profile_root)?;
    let outcome = (|| {
        let profile_id = ProfileId::new(directory.nonce.clone())
            .map_err(|error| invalid_data(error.to_string()))?;
        let extension = brows_application::memory::materialize_fingerprint_probe(
            &directory.path.join("extensions"),
            &profile_id,
            &build.version,
        )?;
        let user_data = directory.path.join("userdata");
        fs::create_dir(&user_data).map_err(|_| internal("无法创建隔离的探针用户目录"))?;
        let server = ProbeServer::start(&directory.nonce)?;
        let output_path = directory.path.join("dump.html");
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)
            .map_err(|_| internal("无法创建探针输出文件"))?;
        let mut command = Command::new(&build.executable_path);
        command.args([
            "--headless=new",
            "--dump-dom",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-sync",
            "--metrics-recording-only",
            "--disable-domain-reliability",
            "--no-proxy-server",
            "--lang=en-US",
        ]);
        command.arg(format!("--user-data-dir={}", user_data.display()));
        command.arg(format!("--load-extension={}", extension.display()));
        command.arg(format!("http://{}/{}", server.address, directory.nonce));
        command
            .stdin(Stdio::null())
            .stdout(Stdio::from(output))
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let child = super::spawn_browser(&mut command).map_err(|_| {
            PortError::new(
                PortErrorKind::Unavailable,
                "无法启动隔离的指纹能力探针；仍可使用系统原生模式",
            )
        })?;
        let mut browser = ProbeChild {
            child,
            reaped: false,
        };
        loop {
            if fs::metadata(&output_path)
                .map_err(|_| internal("无法检查探针输出大小"))?
                .len()
                > MAX_DUMP_BYTES as u64
            {
                return Err(invalid_data("探针输出超过安全大小限制"));
            }
            if let Some(status) = browser
                .child
                .try_wait()
                .map_err(|_| internal("无法查询探针进程状态"))?
            {
                browser.reaped = true;
                if !status.success() {
                    return Err(PortError::new(
                        PortErrorKind::Unavailable,
                        "指纹探针浏览器异常退出；仍可使用系统原生模式",
                    ));
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err(PortError::new(
                    PortErrorKind::Unavailable,
                    format!("指纹能力检测超时；未保留此前的支持状态，可使用系统原生模式（本地页面请求：{}；输出：{} 字节）", server.requested.load(Ordering::Acquire), fs::metadata(&output_path).map(|metadata| metadata.len()).unwrap_or(0)),
                ));
            }
            thread::sleep(Duration::from_millis(40));
        }
        if !server.requested.load(Ordering::Acquire) {
            return Ok(Observation {
                support: FingerprintSupport::Unsupported,
                detail: "探针未观测到本地测试页面，指纹扩展未通过验证；仍可使用系统原生模式"
                    .to_owned(),
            });
        }
        let mut output = Vec::new();
        File::open(&output_path)
            .map_err(|_| internal("无法读取探针输出"))?
            .take(MAX_DUMP_BYTES as u64 + 1)
            .read_to_end(&mut output)
            .map_err(|_| internal("无法读取探针输出"))?;
        match inspect_dump(&output, &directory.nonce) {
            Ok(fields) if fields.is_empty() => Ok(Observation {
                support: FingerprintSupport::Supported,
                detail: "隔离的 headless 本地页面在首个脚本执行时观测到生产指纹扩展的语言、硬件、屏幕和 WebGL 读取覆盖；这不代表全部 API、可见窗口或检测站通过率，系统原生模式仍可用".to_owned(),
            }),
            Ok(fields) => Ok(Observation {
                support: FingerprintSupport::Unsupported,
                detail: format!("未观测到所需 document_start 覆盖（{}），此运行时不能启动 SeedableNoise；仍可使用系统原生模式", fields.join("、")),
            }),
            Err(_) => Ok(Observation {
                support: FingerprintSupport::Unsupported,
                detail: "本地页面未返回有效指纹观测，扩展加载或注入未通过验证；仍可使用系统原生模式".to_owned(),
            }),
        }
    })();
    // Never remove broad roots or follow browser-created symlinks/junctions.
    // A cleanup failure fails the probe rather than hiding leftover processes.
    let cleanup = directory.cleanup();
    match (outcome, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(observation), Ok(())) => Ok(observation),
    }
}

#[cfg(windows)]
#[derive(Debug)]
struct PrivateProbe(super::session_control::Controller);
#[cfg(windows)]
impl Drop for PrivateProbe {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(windows)]
fn run_private(build: &BrowserBuild, profile_root: &Path) -> PortResult<Observation> {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    let directory = ProbeDirectory::create(profile_root)?;
    let outcome = (|| {
        let server = ProbeServer::start_mode(&directory.nonce, true)?;
        let user_data = directory.path.join("userdata");
        fs::create_dir(&user_data).map_err(|_| internal("cannot create private probe data"))?;
        let mut policy = brows_application::memory::fingerprint_probe_policy(&build.version)?;
        // Exercise enabled native touch rather than requiring the host to be
        // physically non-touch. Each real profile still verifies its own value.
        policy.persona.touch_points = 5;
        let mut args: Vec<std::ffi::OsString> = [
            "--headless=new",
            "--no-startup-window",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-sync",
            "--metrics-recording-only",
            "--disable-domain-reliability",
            "--no-proxy-server",
            "--site-per-process",
            "--host-resolver-rules=MAP localhost 127.0.0.1",
            "--lang=pl-PL",
        ]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect();
        args.push(format!("--user-data-dir={}", user_data.display()).into());
        args.push(format!("--user-agent={}", policy.user_agent).into());
        let url = format!("http://{}/{}", server.address, directory.nonce);
        let browser = PrivateProbe(super::session_control::Controller::start(
            &build.executable_path,
            &args,
            policy,
            &[url],
        )?);
        while Instant::now() < deadline {
            let encoded = match browser.0.probe_first_document(deadline) {
                Ok(value) => value,
                Err(error) if error.kind == PortErrorKind::Unavailable => {
                    if browser.0.try_wait().ok().flatten().is_some() {
                        return Err(error);
                    }
                    thread::sleep(Duration::from_millis(25));
                    continue;
                }
                Err(error) => return Err(error),
            };
            if encoded.is_empty() {
                thread::sleep(Duration::from_millis(25));
                continue;
            }
            let dump = format!("{PRE_START}{encoded}</pre>");
            let missing = inspect_private_dump(dump.as_bytes(), &directory.nonce)?;
            return Ok(Observation {
                support: if missing.is_empty() {
                    FingerprintSupport::Supported
                } else {
                    FingerprintSupport::Unsupported
                },
                detail: if missing.is_empty() {
                    "隔离的私有管道在页面首脚本观测到语言、硬件和 WebGL，页面及独立跨进程 iframe 的原生屏幕与 CSS 均匹配；不依赖 MV3，不代表全部 API 或检测站通过率".to_owned()
                } else {
                    format!(
                        "私有管道首脚本配置未通过（{}）；SeedableNoise 保持不可用",
                        missing.join("、")
                    )
                },
            });
        }
        Err(PortError::new(
            PortErrorKind::Unavailable,
            "private first-script probe timed out",
        ))
    })();
    let cleanup = directory.cleanup();
    match (outcome, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(observation), Ok(())) => Ok(observation),
    }
}

#[derive(Debug)]
struct ProbeChild {
    child: Child,
    reaped: bool,
}

impl Drop for ProbeChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        #[cfg(windows)]
        {
            // Only this newly spawned PID tree is targeted; no image-name kill
            // and no user browser process is enumerated or stopped.
            if let Some(system_root) = std::env::var_os("SystemRoot") {
                use std::os::windows::process::CommandExt;
                let mut stop =
                    Command::new(PathBuf::from(system_root).join("System32/taskkill.exe"));
                stop.args(["/PID", &self.child.id().to_string(), "/T", "/F"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(0x0800_0000);
                if let Ok(mut stopper) = stop.spawn() {
                    let deadline = Instant::now() + Duration::from_secs(1);
                    while matches!(stopper.try_wait(), Ok(None)) && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(10));
                    }
                    let _ = stopper.kill();
                    let _ = stopper.wait();
                }
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug)]
struct ProbeServer {
    address: SocketAddr,
    stopped: Arc<AtomicBool>,
    requested: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ProbeServer {
    fn start(nonce: &str) -> PortResult<Self> {
        Self::start_mode(nonce, false)
    }

    fn start_mode(nonce: &str, private: bool) -> PortResult<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(|_| {
            PortError::new(PortErrorKind::Unavailable, "无法启动仅本地可访问的探针页面")
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|_| internal("无法设置探针服务器边界"))?;
        let address = listener
            .local_addr()
            .map_err(|_| internal("无法读取探针本地地址"))?;
        let stopped = Arc::new(AtomicBool::new(false));
        let requested = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let served = requested.clone();
        let route = format!("/{nonce}");
        let page = if private {
            private_probe_page(nonce, address.port())
        } else {
            probe_page(nonce)
        };
        let frame = private.then(|| private_probe_frame(nonce, address.port()));
        let worker = thread::Builder::new()
            .name("fingerprint-probe-http".to_owned())
            .spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, peer)) if peer.ip().is_loopback() => {
                            if serve_request(&mut stream, &route, &page, frame.as_deref()) {
                                served.store(true, Ordering::Release);
                            }
                        }
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|_| internal("无法启动探针服务器线程"))?;
        Ok(Self {
            address,
            stopped,
            requested,
            worker: Some(worker),
        })
    }
}

impl Drop for ProbeServer {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve_request(stream: &mut TcpStream, route: &str, page: &str, frame: Option<&str>) -> bool {
    // Winsock accept inherits the listener's nonblocking mode. Timeouts do
    // not change it, so a browser preconnection could otherwise be closed
    // before its first request bytes arrive.
    if stream.set_nonblocking(false).is_err()
        || stream
            .set_read_timeout(Some(Duration::from_millis(150)))
            .is_err()
        || stream
            .set_write_timeout(Some(Duration::from_millis(150)))
            .is_err()
    {
        return false;
    }
    let mut request = Vec::new();
    let mut chunk = [0; 512];
    let deadline = Instant::now() + Duration::from_millis(300);
    while request.len() < MAX_REQUEST_BYTES && Instant::now() < deadline {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return false,
            Ok(count) => request.extend_from_slice(&chunk[..count]),
        }
        if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            break;
        }
    }
    if request.len() > MAX_REQUEST_BYTES {
        return false;
    }
    let request = String::from_utf8_lossy(&request);
    let mut first_line = request
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    let get = first_line.next() == Some("GET");
    let path = first_line.next();
    let frame_route = format!("{route}/screen");
    let body = if get && path == Some(route) {
        Some(page)
    } else if get && path == Some(frame_route.as_str()) {
        frame
    } else {
        None
    };
    let (status, body, matches) = match body {
        Some(body) => ("200 OK", body, true),
        None => ("404 Not Found", "", false),
    };
    let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}", body.len());
    stream.write_all(response.as_bytes()).is_ok() && matches
}

fn probe_page(nonce: &str) -> String {
    // All values are captured synchronously in the very first page script;
    // only writing the captured observation waits for DOMContentLoaded.
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><script>
{capture}
document.addEventListener('DOMContentLoaded', () => {{
  document.getElementById('brows-fingerprint-probe').textContent =
    Array.from(new TextEncoder().encode(JSON.stringify(result)), b => b.toString(16).padStart(2, '0')).join('');
}});
</script></head><body><pre id="brows-fingerprint-probe"></pre></body></html>"#,
        capture = probe_capture(nonce)
    )
}

fn probe_capture(nonce: &str) -> String {
    format!(
        r#"const result = {{ token: "{nonce}", language: navigator.language,
  languages: Array.from(navigator.languages), concurrency: navigator.hardwareConcurrency,
  memory: navigator.deviceMemory, memoryType: typeof navigator.deviceMemory,
  screenWidth: screen.width, screenHeight: screen.height, webglVendor: null }};
try {{ const gl = document.createElement('canvas').getContext('webgl');
  const debug = gl && gl.getExtension('WEBGL_debug_renderer_info');
  if (debug) result.webglVendor = gl.getParameter(debug.UNMASKED_VENDOR_WEBGL);
}} catch (_) {{}}"#
    )
}

const NATIVE_SCREEN_CAPTURE: &str = "(()=>{const names=['width','height','availWidth','availHeight'];return {width:screen.width,height:screen.height,availWidth:screen.availWidth,availHeight:screen.availHeight,nativeGetters:names.every(name=>{const get=Object.getOwnPropertyDescriptor(Screen.prototype,name).get;let receiver=false;try{Reflect.apply(get,{},[])}catch(e){receiver=e instanceof TypeError}return receiver&&Function.prototype.toString.call(get).includes('[native code]')&&Reflect.apply(get,screen,[])===screen[name]}),deviceMedia:matchMedia('(device-width: 1777px) and (device-height: 1111px)').matches}})()";

fn private_probe_page(nonce: &str, port: u16) -> String {
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><script>
{capture}
result.screenTop={screen};
let frame,ready=false;
const emit=()=>{{if(ready&&result.screenCrossFrame)document.getElementById('brows-fingerprint-probe').textContent=Array.from(new TextEncoder().encode(JSON.stringify(result)),b=>b.toString(16).padStart(2,'0')).join('');}};
addEventListener('message',event=>{{if(frame&&event.source===frame.contentWindow&&event.origin==='http://localhost:{port}'&&event.data?.token==='{nonce}'&&event.data.screen){{result.screenCrossFrame=event.data.screen;emit();}}}});
addEventListener('DOMContentLoaded',()=>{{frame=document.getElementById('owned-screen-frame');ready=true;frame.src='http://localhost:{port}/{nonce}/screen';emit();}});
</script></head><body><pre id="brows-fingerprint-probe"></pre><iframe id="owned-screen-frame"></iframe></body></html>"#,
        capture = probe_capture(nonce),
        screen = NATIVE_SCREEN_CAPTURE
    )
}

fn private_probe_frame(nonce: &str, port: u16) -> String {
    format!(
        r#"<!doctype html><script>const proof={{token:'{nonce}',screen:{screen}}};parent.postMessage(proof,'http://127.0.0.1:{port}');</script>"#,
        screen = NATIVE_SCREEN_CAPTURE
    )
}

fn decode_dump(output: &[u8], nonce: &str) -> PortResult<Value> {
    if output.len() > MAX_DUMP_BYTES {
        return Err(invalid_data("probe output exceeds limit"));
    }
    let dom = std::str::from_utf8(output).map_err(|_| invalid_data("probe output is not UTF-8"))?;
    if dom.matches(PRE_START).count() != 1 {
        return Err(invalid_data(
            "probe observation marker is missing or duplicated",
        ));
    }
    let encoded = dom
        .split_once(PRE_START)
        .unwrap()
        .1
        .split_once("</pre>")
        .ok_or_else(|| invalid_data("probe observation is incomplete"))?
        .0
        .trim();
    if encoded.is_empty() || encoded.len() % 2 != 0 || encoded.len() > 8192 {
        return Err(invalid_data("invalid probe observation size"));
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let hex = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        let high = hex(pair[0]).ok_or_else(|| invalid_data("probe observation is not hex"))?;
        let low = hex(pair[1]).ok_or_else(|| invalid_data("probe observation is not hex"))?;
        bytes.push((high << 4) | low);
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| invalid_data("probe observation is invalid JSON"))?;
    if value["token"].as_str() != Some(nonce) {
        return Err(invalid_data("probe observation is from a different run"));
    }
    Ok(value)
}

fn inspect_dump(output: &[u8], nonce: &str) -> PortResult<Vec<&'static str>> {
    Ok(inspect_captured_fields(&decode_dump(output, nonce)?))
}

#[cfg(any(windows, test))]
fn inspect_private_dump(output: &[u8], nonce: &str) -> PortResult<Vec<&'static str>> {
    let value = decode_dump(output, nonce)?;
    let mut missing = inspect_captured_fields(&value);
    for name in ["screenTop", "screenCrossFrame"] {
        let screen = &value[name];
        if screen["width"].as_u64() != Some(1777)
            || screen["height"].as_u64() != Some(1111)
            || screen["availWidth"].as_u64() != Some(1777)
            || screen["availHeight"].as_u64() != Some(1111)
            || screen["nativeGetters"] != true
            || screen["deviceMedia"] != true
        {
            missing.push(name);
        }
    }
    Ok(missing)
}

fn inspect_captured_fields(value: &Value) -> Vec<&'static str> {
    let mut missing = Vec::new();
    for (name, matches) in [
        ("language", value["language"].as_str() == Some("pl-PL")),
        (
            "languages",
            value["languages"] == serde_json::json!(["pl-PL", "pl"]),
        ),
        (
            "hardwareConcurrency",
            value["concurrency"].as_u64() == Some(3),
        ),
        (
            "deviceMemory",
            value["memory"].as_f64() == Some(0.5) && value["memoryType"].as_str() == Some("number"),
        ),
        (
            "screen",
            value["screenWidth"].as_u64() == Some(1777)
                && value["screenHeight"].as_u64() == Some(1111),
        ),
        (
            "webglVendor",
            value["webglVendor"].as_str() == Some("Google Inc. (Intel)"),
        ),
    ] {
        if !matches {
            missing.push(name);
        }
    }
    missing
}

#[derive(Debug)]
struct ProbeDirectory {
    root: PathBuf,
    path: PathBuf,
    nonce: String,
}

impl ProbeDirectory {
    fn create(root: &Path) -> PortResult<Self> {
        if !reject_link(root)?.is_dir() {
            return Err(invalid_data("probe root is not a directory"));
        }
        let canonical = root
            .canonicalize()
            .map_err(|_| invalid_data("probe root is unavailable"))?;
        if canonical != root {
            return Err(invalid_data("managed probe root was replaced"));
        }
        let root = canonical;
        for _ in 0..16 {
            let nonce = format!(
                "fingerprint-probe-{}-{}-{}",
                std::process::id(),
                now_ms(),
                NEXT_PROBE.fetch_add(1, Ordering::Relaxed)
            );
            let path = root.join(&nonce);
            match fs::create_dir(&path) {
                Ok(()) => {
                    let mut marker = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(path.join(MARKER_FILE))
                        .map_err(|_| internal("cannot establish ownership of probe directory"))?;
                    marker
                        .write_all(nonce.as_bytes())
                        .map_err(|_| internal("cannot establish ownership of probe directory"))?;
                    return Ok(Self { root, path, nonce });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(internal("cannot create isolated probe directory")),
            }
        }
        Err(internal("cannot allocate unique probe directory"))
    }

    fn cleanup(&self) -> PortResult<()> {
        if self.path.parent() != Some(self.root.as_path())
            || self.path.file_name().and_then(|name| name.to_str()) != Some(self.nonce.as_str())
            || !self.nonce.starts_with("fingerprint-probe-")
        {
            return Err(invalid_data("refusing unsafe probe cleanup target"));
        }
        reject_link(&self.root)?;
        reject_link(&self.path)?;
        if self.root.canonicalize().ok().as_ref() != Some(&self.root)
            || self.path.canonicalize().ok().as_ref() != Some(&self.path)
        {
            return Err(invalid_data("probe cleanup target changed"));
        }
        let marker_path = self.path.join(MARKER_FILE);
        reject_link(&marker_path)?;
        let mut marker = Vec::new();
        File::open(&marker_path)
            .map_err(|_| invalid_data("probe ownership marker is missing"))?
            .take(256)
            .read_to_end(&mut marker)
            .map_err(|_| invalid_data("cannot inspect probe ownership marker"))?;
        if marker != self.nonce.as_bytes() {
            return Err(invalid_data("probe ownership marker changed"));
        }
        let mut budget = 10_000;
        let deadline = Instant::now() + Duration::from_secs(2);
        validate_tree(&self.path, &self.path, 0, &mut budget, deadline)?;
        fs::remove_dir_all(&self.path).map_err(|_| {
            PortError::new(
                PortErrorKind::Unavailable,
                "隔离探针目录暂时无法安全清理；未保留支持状态，不影响现有环境",
            )
        })
    }
}

fn reject_link(path: &Path) -> PortResult<fs::Metadata> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| invalid_data("cannot inspect probe path"))?;
    if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
        return Err(invalid_data("probe path contains a link or reparse point"));
    }
    Ok(metadata)
}

fn validate_tree(
    path: &Path,
    owned: &Path,
    depth: usize,
    budget: &mut usize,
    deadline: Instant,
) -> PortResult<()> {
    if depth > 64 || *budget == 0 || Instant::now() >= deadline {
        return Err(invalid_data("probe cleanup tree exceeds limits"));
    }
    *budget -= 1;
    let metadata = reject_link(path)?;
    let canonical = path
        .canonicalize()
        .map_err(|_| invalid_data("cannot resolve probe cleanup path"))?;
    if !canonical.starts_with(owned) {
        return Err(invalid_data("probe cleanup path left owned directory"));
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)
            .map_err(|_| invalid_data("cannot inspect probe cleanup directory"))?
        {
            let entry = entry.map_err(|_| invalid_data("cannot inspect probe cleanup entry"))?;
            validate_tree(&entry.path(), owned, depth + 1, budget, deadline)?;
        }
    } else if !metadata.is_file() {
        return Err(invalid_data("probe cleanup contains a non-regular entry"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    #[ignore = "requires signed Edge; actual private probe must reject the stock OOPIF screen gap"]
    fn private_probe_rejects_stock_oopif_screen() {
        use brows_domain::{BrowserCapabilities, BrowserChannel, RuntimeTrust};
        let executable = PathBuf::from(
            std::env::var_os("BROWS_ACCEPTANCE_BROWSER").expect("explicit acceptance browser"),
        );
        let publisher =
            crate::verify_authenticode_publisher(&executable, &["Microsoft Corporation"]).unwrap();
        let build = BrowserBuild {
            id: BrowserBuildId::new("owned-stock-screen-negative").unwrap(),
            provider: "isolated signed Edge test".to_owned(),
            version: std::env::var("BROWS_ACCEPTANCE_VERSION").expect("verified binary version"),
            channel: BrowserChannel::Stable,
            sha256: crate::digest_file(&executable).unwrap(),
            executable_path: executable,
            publisher,
            trust: RuntimeTrust::Verified,
            capabilities: BrowserCapabilities {
                supports_devtools_pipe: true,
                ..BrowserCapabilities::default()
            },
            installed_at_ms: now_ms(),
        };
        let root = temp_root("private-screen-negative");
        let observed = run_private(&build, &root);
        let passed = observed.as_ref().is_ok_and(|value| {
            value.support == FingerprintSupport::Unsupported
                && value.detail.contains("screenCrossFrame")
        });
        let report = serde_json::json!({"scope":"actual private capability probe on signed Edge",
            "assertionsPassed":passed,"expected":"Unsupported: screenCrossFrame",
            "detail":observed.as_ref().map(|value|value.detail.as_str()).unwrap_or("probe error"),
            "nativeScreenPassed":false,"runtimeAdmitted":false,"complete":false});
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/native-screen-capability-negative.json");
        fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        fs::remove_dir(&root).unwrap();
        assert!(passed, "{report}");
    }

    #[test]
    fn probe_http_accepts_delayed_headers_on_a_nonblocking_accepted_socket() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (send, ready) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_nonblocking(true).unwrap();
            send.send(()).unwrap();
            serve_request(&mut stream, "/delayed", "probe-ok", None)
        });
        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        thread::sleep(Duration::from_millis(50));
        client
            .write_all(b"GET /delayed HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(server.join().unwrap());
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.ends_with("probe-ok"));
    }

    fn temp_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "brows-probe-{name}-{}-{}-{}",
            std::process::id(),
            now_ms(),
            NEXT_PROBE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path.canonicalize().unwrap()
    }

    fn encoded_dump(value: &Value) -> Vec<u8> {
        let hex = serde_json::to_vec(value)
            .unwrap()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!("<html><body>{PRE_START}{hex}</pre></body></html>").into_bytes()
    }

    fn observation() -> Value {
        serde_json::json!({ "token":"probe-token", "language":"pl-PL", "languages":["pl-PL","pl"],
            "concurrency":3, "memory":0.5, "memoryType":"number", "screenWidth":1777,
            "screenHeight":1111, "webglVendor":"Google Inc. (Intel)" })
    }

    #[test]
    fn private_parser_rejects_host_screen_and_script_fallback_in_cross_frame() {
        let mut value = observation();
        let screen = serde_json::json!({"width":1777,"height":1111,
            "availWidth":1777,"availHeight":1111,"nativeGetters":true,"deviceMedia":true});
        value["screenTop"] = screen.clone();
        value["screenCrossFrame"] = screen;
        assert!(inspect_private_dump(&encoded_dump(&value), "probe-token")
            .unwrap()
            .is_empty());
        for (field, incorrect) in [
            ("width", serde_json::json!(656)),
            ("nativeGetters", serde_json::json!(false)),
            ("deviceMedia", serde_json::json!(false)),
        ] {
            let mut failed = value.clone();
            failed["screenCrossFrame"][field] = incorrect;
            assert_eq!(
                inspect_private_dump(&encoded_dump(&failed), "probe-token").unwrap(),
                ["screenCrossFrame"]
            );
        }
        value.as_object_mut().unwrap().remove("screenCrossFrame");
        assert_eq!(
            inspect_private_dump(&encoded_dump(&value), "probe-token").unwrap(),
            ["screenCrossFrame"]
        );
    }

    #[test]
    fn parser_requires_all_sentinels_and_numeric_memory() {
        assert!(inspect_dump(&encoded_dump(&observation()), "probe-token")
            .unwrap()
            .is_empty());
        let mut value = observation();
        value["memory"] = serde_json::json!("0.5");
        assert_eq!(
            inspect_dump(&encoded_dump(&value), "probe-token").unwrap(),
            ["deviceMemory"]
        );
        value = observation();
        value["webglVendor"] = Value::Null;
        assert_eq!(
            inspect_dump(&encoded_dump(&value), "probe-token").unwrap(),
            ["webglVendor"]
        );
    }

    #[test]
    fn parser_rejects_stale_duplicate_malformed_and_oversized_output() {
        assert!(inspect_dump(&encoded_dump(&observation()), "wrong-token").is_err());
        assert!(inspect_dump(
            format!("{PRE_START}00</pre>{PRE_START}00</pre>").as_bytes(),
            "probe-token"
        )
        .is_err());
        assert!(inspect_dump(format!("{PRE_START}xz</pre>").as_bytes(), "probe-token").is_err());
        assert!(inspect_dump(&vec![b'x'; MAX_DUMP_BYTES + 1], "probe-token").is_err());
    }

    #[test]
    fn one_probe_slot_per_build_is_released_on_drop() {
        let slots = Mutex::new(HashSet::new());
        let id = BrowserBuildId::new("test-build").unwrap();
        let first = ProbeSlot::acquire(&slots, &id).unwrap();
        assert_eq!(
            ProbeSlot::acquire(&slots, &id).unwrap_err().kind,
            PortErrorKind::AlreadyExists
        );
        drop(first);
        assert!(ProbeSlot::acquire(&slots, &id).is_ok());
    }

    #[test]
    fn cleanup_only_removes_the_exact_owned_probe_directory() {
        let root = temp_root("cleanup");
        let directory = ProbeDirectory::create(&root).unwrap();
        let kept = root.join("existing-profile");
        fs::create_dir(&kept).unwrap();
        fs::write(kept.join("keep.txt"), b"user data").unwrap();
        let forged = ProbeDirectory {
            root: root.clone(),
            path: kept.clone(),
            nonce: "fingerprint-probe-forged".to_owned(),
        };
        assert!(forged.cleanup().is_err());
        let owned = directory.path.clone();
        directory.cleanup().unwrap();
        assert!(!owned.exists());
        assert!(kept.join("keep.txt").exists());
    }

    #[test]
    fn cleanup_rejects_changed_ownership_without_removing_the_directory() {
        let root = temp_root("changed-owner");
        let directory = ProbeDirectory::create(&root).unwrap();
        fs::write(directory.path.join(MARKER_FILE), b"not owned by this probe").unwrap();
        assert!(directory.cleanup().is_err());
        assert!(directory.path.exists());
        fs::write(directory.path.join(MARKER_FILE), directory.nonce.as_bytes()).unwrap();
        directory.cleanup().unwrap();
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires Windows symlink creation privilege"]
    fn cleanup_rejects_windows_reparse_points() {
        let root = temp_root("windows-link");
        let directory = ProbeDirectory::create(&root).unwrap();
        let external = root.join("external");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("keep.txt"), b"keep").unwrap();
        let linked = directory.path.join("linked");
        std::os::windows::fs::symlink_dir(&external, &linked).unwrap();
        assert!(directory.cleanup().is_err());
        assert!(external.join("keep.txt").exists());
        fs::remove_dir(linked).unwrap();
        directory.cleanup().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_rejects_symlinks_without_touching_external_data() {
        let root = temp_root("link");
        let directory = ProbeDirectory::create(&root).unwrap();
        let outside = root.join("external");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep.txt"), b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, directory.path.join("linked")).unwrap();
        assert!(directory.cleanup().is_err());
        assert!(outside.join("keep.txt").exists());
        fs::remove_file(directory.path.join("linked")).unwrap();
        directory.cleanup().unwrap();
    }

    #[test]
    fn page_captures_values_before_dom_ready_and_uses_no_external_sources() {
        let page = probe_page("safe-nonce");
        assert!(
            page.find("language: navigator.language").unwrap()
                < page.find("DOMContentLoaded").unwrap()
        );
        assert!(!page.contains("https://"));
        assert!(!page.contains("fetch("));
    }
}
