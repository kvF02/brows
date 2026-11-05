//! Session-scoped control and observation over private inherited pipes.
//! No generic command/evaluate surface is exposed through application ports.
use super::{
    internal, invalid_data, now_ms,
    target_registry::{Target, TargetJob, Targets, MAX_TARGETS},
    win_pipe,
};
use brows_application::{
    PortError, PortErrorKind, PortResult, SessionObservation, TargetObservation,
};
use brows_domain::{PlatformKind, SessionFingerprintConfig, SessionId};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    io::{Read, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const TIMEOUT: Duration = Duration::from_secs(8);
const MAX_FRAME: usize = 2 * 1024 * 1024;
const POLICY_WORKERS: usize = 8;
const TARGET_EXECUTION_ENDED: &str = "browser target execution ended";
#[derive(Debug)]
struct Wire {
    process: Arc<win_pipe::Process>,
    next: AtomicU64,
    closing: AtomicBool,
    version: Mutex<String>,
    policy_failure: Mutex<Option<String>>,
    writer: mpsc::SyncSender<Vec<u8>>,
    pending: Arc<Mutex<BTreeMap<u64, mpsc::Sender<Value>>>>,
    paused: Mutex<BTreeSet<String>>,
    pause_ready: Condvar,
}
#[derive(Debug)]
struct PendingReply {
    id: u64,
    method: String,
    receive: mpsc::Receiver<Value>,
    session: Option<String>,
    pending: Arc<Mutex<BTreeMap<u64, mpsc::Sender<Value>>>>,
}
impl PendingReply {
    fn receive_until(&self, deadline: Instant) -> Result<Value, mpsc::RecvTimeoutError> {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(mpsc::RecvTimeoutError::Timeout)?;
        self.receive.recv_timeout(remaining)
    }
}
impl Drop for PendingReply {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&self.id);
        }
    }
}
impl Wire {
    fn fail(&self) {
        self.closing.store(true, Ordering::Release);
        let _ = self.process.kill();
        if let Ok(mut p) = self.pending.lock() {
            p.clear();
        }
    }
    fn ensure_open(&self) -> PortResult<()> {
        if self.closing.load(Ordering::Acquire) {
            return Err(internal(
                self.policy_failure
                    .lock()
                    .ok()
                    .and_then(|reason| reason.clone())
                    .unwrap_or_else(|| "session control is closed".to_owned()),
            ));
        }
        Ok(())
    }
    fn begin(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> PortResult<PendingReply> {
        self.ensure_open()?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut frame = json!({"id":id,"method":method,"params":params});
        if let Some(s) = session {
            frame["sessionId"] = json!(s);
        }
        let mut bytes =
            serde_json::to_vec(&frame).map_err(|_| internal("cannot encode session policy"))?;
        bytes.push(0);
        if bytes.len() > 64 * 1024 {
            return Err(invalid_data("session control message exceeds limit"));
        }
        let (send, receive) = mpsc::channel();
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| internal("session replies lock poisoned"))?;
            if pending.len() >= 64 {
                return Err(internal("too many pending session commands"));
            }
            pending.insert(id, send);
        }
        if self.writer.try_send(bytes).is_err() {
            self.fail();
            return Err(internal("session control writer unavailable"));
        }
        Ok(PendingReply {
            id,
            method: method.to_owned(),
            receive,
            session: session.map(str::to_owned),
            pending: self.pending.clone(),
        })
    }
    fn finish(&self, reply: PendingReply) -> PortResult<Value> {
        self.finish_until(reply, Instant::now() + TIMEOUT)
    }
    fn finish_until(&self, reply: PendingReply, deadline: Instant) -> PortResult<Value> {
        let method = &reply.method;
        let result = reply.receive_until(deadline);
        let value = result.map_err(|_| {
            if reply.session.is_none() {
                self.fail();
            }
            PortError::new(
                PortErrorKind::Unavailable,
                format!("session control response timeout: {method}"),
            )
        })?;
        if let Some(error) = value.get("error") {
            if target_execution_ended(error) {
                return Err(PortError::new(
                    PortErrorKind::Unavailable,
                    TARGET_EXECUTION_ENDED,
                ));
            }
            if method == "Emulation.setTimezoneOverride"
                && error["message"].as_str().is_some_and(|message| {
                    message.trim_end_matches('.') == "Timezone override is already in effect"
                })
            {
                return Err(PortError::new(
                    PortErrorKind::AlreadyExists,
                    "native timezone override is already active",
                ));
            }
            if error["code"] == -32001
                || matches!(
                    error["message"].as_str(),
                    Some("Session with given id not found." | "No target with given id found")
                )
            {
                return Err(PortError::not_found("browser target"));
            }
            let reason = if matches!(
                method.as_str(),
                "Runtime.runIfWaitingForDebugger"
                    | "Debugger.enable"
                    | "Debugger.pause"
                    | "Debugger.resume"
                    | "Debugger.disable"
            ) {
                error["message"]
                    .as_str()
                    .filter(|message| {
                        message.len() <= 160
                            && message
                                .bytes()
                                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
                    })
                    .unwrap_or("unavailable")
            } else {
                "redacted"
            };
            return Err(internal(format!(
                "browser cannot apply {method} (code {}; {reason})",
                error["code"].as_i64().unwrap_or(0)
            )));
        }
        Ok(value["result"].clone())
    }
    fn call(&self, method: &str, params: Value, session: Option<&str>) -> PortResult<Value> {
        self.finish(self.begin(method, params, session)?)
    }
    fn pause_service_worker(&self, session: &str) -> PortResult<()> {
        self.paused
            .lock()
            .map_err(|_| internal("pause lock poisoned"))?
            .remove(session);
        // Startup must be released so the Service Worker agent can initialize.
        // Queue a V8 pause first; its first statement stays blocked until policy
        // installation has completed. No call-frame data is retained.
        let enabled = self.begin("Debugger.enable", json!({}), Some(session))?;
        let stopped = self.begin("Debugger.pause", json!({}), Some(session))?;
        self.call("Runtime.runIfWaitingForDebugger", json!({}), Some(session))?;
        self.finish(enabled)?;
        self.finish(stopped)?;
        let deadline = Instant::now() + TIMEOUT;
        let mut paused = self
            .paused
            .lock()
            .map_err(|_| internal("pause lock poisoned"))?;
        while !paused.contains(session) {
            if self.closing.load(Ordering::Acquire) || Instant::now() >= deadline {
                // The event actor may have invalidated this generation while
                // startup was pending. The policy worker decides whether this
                // error still belongs to an active target and must fail closed.
                return Err(internal(
                    "Service Worker did not pause before its first script",
                ));
            }
            paused = self
                .pause_ready
                .wait_timeout(paused, Duration::from_millis(100))
                .map_err(|_| internal("pause lock poisoned"))?
                .0;
        }
        Ok(())
    }
}
#[derive(Debug)]
pub(super) struct Controller {
    wire: Arc<Wire>,
    targets: Arc<(Mutex<Targets>, Condvar)>,
    threads: Vec<JoinHandle<()>>,
}
impl Controller {
    pub(super) fn start(
        executable: &Path,
        args: &[OsString],
        policy: SessionFingerprintConfig,
        urls: &[String],
    ) -> PortResult<Self> {
        let (process, mut output, mut input) = win_pipe::spawn(executable, args)
            .map_err(|_| internal("cannot start private session control"))?;
        let (writer, write_receive) = mpsc::sync_channel::<Vec<u8>>(64);
        let (events, event_receive) = mpsc::sync_channel::<Value>(128);
        let wire = Arc::new(Wire {
            process: Arc::new(process),
            next: AtomicU64::new(1),
            closing: AtomicBool::new(false),
            version: Mutex::new(String::new()),
            policy_failure: Mutex::new(None),
            writer,
            pending: Arc::new(Mutex::new(BTreeMap::new())),
            paused: Mutex::new(BTreeSet::new()),
            pause_ready: Condvar::new(),
        });
        let targets = Arc::new((Mutex::new(Targets::default()), Condvar::new()));
        let w = wire.clone();
        let write_thread = thread::spawn(move || {
            while !w.closing.load(Ordering::Acquire) {
                match write_receive.recv_timeout(Duration::from_millis(200)) {
                    Ok(frame) => {
                        if output.write_all(&frame).is_err() {
                            w.fail();
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break,
                }
            }
        });
        let w = wire.clone();
        let read_thread = thread::spawn(move || {
            let mut frame = Vec::new();
            let mut chunk = [0u8; 4096];
            'read: loop {
                let size = match input.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(size) => size,
                };
                for byte in &chunk[..size] {
                    if *byte == 0 {
                        let value: Value = match serde_json::from_slice(&frame) {
                            Ok(v) => v,
                            Err(_) => break 'read,
                        };
                        frame.clear();
                        if let Some(id) = value["id"].as_u64() {
                            if let Ok(mut pending) = w.pending.lock() {
                                if let Some(sender) = pending.remove(&id) {
                                    let _ = sender.send(value);
                                }
                            }
                        } else if value["method"] == "Debugger.paused" {
                            if let Some(session) = value["sessionId"].as_str() {
                                if let Ok(mut paused) = w.paused.lock() {
                                    if paused.len() >= MAX_TARGETS {
                                        break 'read;
                                    }
                                    paused.insert(session.to_owned());
                                    w.pause_ready.notify_all();
                                }
                            }
                        } else if matches!(
                            value["method"].as_str(),
                            Some(
                                "Target.attachedToTarget"
                                    | "Target.detachedFromTarget"
                                    | "Inspector.targetCrashed"
                                    | "Inspector.targetReloadedAfterCrash"
                            )
                        ) && events.try_send(value).is_err()
                        {
                            break 'read;
                        }
                    } else {
                        frame.push(*byte);
                        if frame.len() > MAX_FRAME {
                            break 'read;
                        }
                    }
                }
            }
            w.fail();
        });
        // A service-worker startup can depend on another paused target. Keep
        // event dispatch independent of bounded, concurrent policy jobs.
        let (jobs, job_receive) = mpsc::sync_channel::<TargetJob>(MAX_TARGETS);
        let job_receive = Arc::new(Mutex::new(job_receive));
        let policy = Arc::new(policy);
        let mut threads = vec![write_thread, read_thread];
        for _ in 0..POLICY_WORKERS {
            let w = wire.clone();
            let t = targets.clone();
            let receive = job_receive.clone();
            let policy = policy.clone();
            threads.push(thread::spawn(move || {
                while !w.closing.load(Ordering::Acquire) {
                    let job = match receive.lock() {
                        Ok(receiver) => receiver.recv_timeout(Duration::from_millis(200)),
                        Err(_) => {
                            w.fail();
                            break;
                        }
                    };
                    let job = match job {
                        Ok(job) => job,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(_) => break,
                    };
                    let Ok(_lease) = job.gate.lock() else {
                        w.fail();
                        break;
                    };
                    if !t
                        .0
                        .lock()
                        .map(|state| state.is_current(&job))
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let session = &job.target.session;
                    let result: PortResult<()> = (|| {
                        if !job.extension {
                            if job.target.kind == "service_worker" {
                                w.pause_service_worker(session)?;
                            }
                            configure(&w, session, &job.target.kind, &policy)?;
                            if !t
                                .0
                                .lock()
                                .map(|state| state.is_current(&job))
                                .unwrap_or(false)
                            {
                                return Ok(());
                            }
                            if job.target.kind == "service_worker" {
                                w.call("Debugger.resume", json!({}), Some(session))?;
                                w.call("Debugger.disable", json!({}), Some(session))?;
                                w.paused
                                    .lock()
                                    .map_err(|_| internal("pause lock poisoned"))?
                                    .remove(session);
                            }
                        }
                        if job.extension || job.target.kind != "service_worker" {
                            w.call("Runtime.runIfWaitingForDebugger", json!({}), Some(session))?;
                        }
                        Ok(())
                    })();
                    if let Err(error) = result {
                        let Ok(mut state) = t.0.lock() else {
                            w.fail();
                            break;
                        };
                        if !state.is_current(&job) {
                            continue;
                        }
                        if job.target.kind == "service_worker"
                            && error.kind == PortErrorKind::Unavailable
                            && error.message == TARGET_EXECUTION_ENDED
                        {
                            if state.suspend(session).is_err() {
                                w.fail();
                                break;
                            }
                            if let Ok(mut paused) = w.paused.lock() {
                                paused.remove(session);
                            }
                            t.1.notify_all();
                            continue;
                        }
                        if error.kind == PortErrorKind::NotFound {
                            state.detach(session);
                            continue;
                        }
                        if let Ok(mut reason) = w.policy_failure.lock() {
                            if reason.is_none() && error.message != "session control is closed" {
                                *reason = Some(format!(
                                    "{} policy failed: {}",
                                    job.target.kind, error.message
                                ));
                            }
                        }
                        w.fail();
                        t.1.notify_all();
                        break;
                    }
                    let Ok(mut state) = t.0.lock() else {
                        w.fail();
                        break;
                    };
                    if !job.extension && state.ready(&job) {
                        t.1.notify_all();
                    }
                }
            }));
        }
        let w = wire.clone();
        let t = targets.clone();
        threads.push(thread::spawn(move || {
            while !w.closing.load(Ordering::Acquire) {
                let event = match event_receive.recv_timeout(Duration::from_millis(200)) {
                    Ok(v) => v,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                };
                if event["method"] == "Target.detachedFromTarget" {
                    if let Some(session) = event["params"]["sessionId"].as_str() {
                        if let Ok(mut paused) = w.paused.lock() {
                            paused.remove(session);
                        }
                        if let Ok(mut state) = t.0.lock() {
                            state.detach(session);
                        }
                    }
                    continue;
                }
                if matches!(
                    event["method"].as_str(),
                    Some("Inspector.targetCrashed" | "Inspector.targetReloadedAfterCrash")
                ) {
                    let Some(session) = event["sessionId"].as_str() else {
                        w.fail();
                        break;
                    };
                    if event["method"] == "Inspector.targetCrashed" {
                        if let Ok(mut paused) = w.paused.lock() {
                            paused.remove(session);
                        }
                    }
                    let outcome = match t.0.lock() {
                        Ok(mut state) => {
                            if event["method"] == "Inspector.targetCrashed" {
                                state.suspend(session).map(|_| None)
                            } else {
                                state.reload(session)
                            }
                        }
                        Err(_) => {
                            w.fail();
                            break;
                        }
                    };
                    match outcome {
                        Ok(Some(job)) => {
                            if let Ok(mut paused) = w.paused.lock() {
                                paused.remove(session);
                            }
                            if jobs.try_send(job).is_err() {
                                w.fail();
                                break;
                            }
                        }
                        Ok(None) => {}
                        Err(_) => {
                            w.fail();
                            break;
                        }
                    }
                    t.1.notify_all();
                    continue;
                }
                let params = &event["params"];
                let Some(session) = params["sessionId"].as_str() else {
                    w.fail();
                    break;
                };
                let Some(id) = params["targetInfo"]["targetId"].as_str() else {
                    w.fail();
                    break;
                };
                let kind = params["targetInfo"]["type"].as_str().unwrap_or("");
                let extension = params["targetInfo"]["url"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("chrome-extension://");
                let job = match t.0.lock() {
                    Ok(mut state) => state.attach(id, session, kind, extension),
                    Err(_) => {
                        w.fail();
                        break;
                    }
                };
                let job = match job {
                    Ok(Some(job)) => job,
                    Ok(None) => continue,
                    Err(_) => {
                        w.fail();
                        break;
                    }
                };
                if jobs.try_send(job).is_err() {
                    w.fail();
                    break;
                }
            }
        }));
        let controller = Self {
            wire,
            targets,
            threads,
        };
        let version = controller
            .wire
            .call("Browser.getVersion", json!({}), None)?;
        let actual = version["product"]
            .as_str()
            .and_then(|v| v.split('/').nth(1))
            .filter(|v| {
                v.split('.')
                    .all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()))
            })
            .ok_or_else(|| internal("cannot verify running browser version"))?;
        *controller
            .wire
            .version
            .lock()
            .map_err(|_| internal("version lock poisoned"))? = actual.to_owned();
        controller
            .wire
            .call("Target.setAutoAttach", auto_attach(true), None)?;
        let startup_urls = if urls.is_empty() {
            vec!["about:blank".to_owned()]
        } else {
            urls.to_vec()
        };
        let headless = args
            .iter()
            .any(|argument| argument.to_string_lossy().starts_with("--headless"));
        for (index, url) in startup_urls.into_iter().enumerate() {
            let mut params = json!({"url":"about:blank"});
            if !headless && index == 0 {
                params["newWindow"] = json!(true);
            }
            let target = controller.wire.call("Target.createTarget", params, None)?;
            let id = target["targetId"]
                .as_str()
                .ok_or_else(|| internal("browser omitted target identity"))?;
            let target = controller.await_target(id)?;
            if url != "about:blank" {
                controller
                    .wire
                    .call("Page.navigate", json!({"url":url}), Some(&target.session))?;
            }
        }
        Ok(controller)
    }
    fn await_target(&self, id: &str) -> PortResult<Target> {
        let deadline = Instant::now() + TIMEOUT;
        let mut targets = self
            .targets
            .0
            .lock()
            .map_err(|_| internal("target lock poisoned"))?;
        loop {
            if let Some(target) = targets.get(id) {
                return Ok(target.clone());
            }
            if self.wire.closing.load(Ordering::Acquire) || Instant::now() >= deadline {
                return Err(internal(
                    self.wire
                        .policy_failure
                        .lock()
                        .ok()
                        .and_then(|v| v.clone())
                        .unwrap_or_else(|| {
                            "browser target policy failed before navigation".to_owned()
                        }),
                ));
            }
            targets = self
                .targets
                .1
                .wait_timeout(targets, Duration::from_millis(100))
                .map_err(|_| internal("target lock poisoned"))?
                .0;
        }
    }
    pub(super) fn id(&self) -> u32 {
        self.wire.process.id()
    }
    pub(super) fn kill(&self) -> std::io::Result<()> {
        self.wire.fail();
        Ok(())
    }
    pub(super) fn wait(&self) -> std::io::Result<()> {
        self.wire.process.wait()
    }
    pub(super) fn try_wait(&self) -> std::io::Result<Option<u32>> {
        self.wire.process.try_wait()
    }
    pub(super) fn probe_network(
        &self,
        config: &brows_application::NetworkProbeConfig,
    ) -> PortResult<brows_application::NetworkProbeObservation> {
        config.validate()?;
        let target = self.wire.call(
            "Target.createTarget",
            json!({"url":"about:blank","background":true}),
            None,
        )?;
        let id = target["targetId"]
            .as_str()
            .ok_or_else(|| internal("probe target identity missing"))?;
        let outcome = (|| {
            let target = self.await_target(id)?;
            let service = json!({"origin":config.origin,"zone":config.zone,"stunMin":config.stun_min,"stunMax":config.stun_max});
            let script =
                include_str!("network_probe.js").replace("__BROWS_SERVICE__", &service.to_string());
            evaluate(&self.wire, &target.session, &script)?;
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline {
                let value = evaluate(
                    &self.wire,
                    &target.session,
                    "globalThis.__browsNetworkProbe ?? null",
                )?;
                if !value.is_null() {
                    let status = |key: &str| -> PortResult<String> {
                        value[key]
                            .as_str()
                            .filter(|s| matches!(*s, "pass" | "fail" | "not_checked"))
                            .map(str::to_owned)
                            .ok_or_else(|| internal("invalid network probe status"))
                    };
                    return Ok(brows_application::NetworkProbeObservation {
                        dns: status("dns")?,
                        web_rtc: status("webRtc")?,
                        http_egress: status("httpEgress")?,
                    });
                }
                thread::sleep(Duration::from_millis(100));
            }
            Ok(brows_application::NetworkProbeObservation {
                dns: "not_checked".to_owned(),
                web_rtc: "not_checked".to_owned(),
                http_egress: "not_checked".to_owned(),
            })
        })();
        let _ = self
            .wire
            .call("Target.closeTarget", json!({"targetId":id}), None);
        outcome
    }
    /// Read only the first-script capture in this fresh probe's sole page.
    /// This is an internal fixed read, never an application IPC expression.
    pub(super) fn probe_first_document(&self, deadline: Instant) -> PortResult<String> {
        self.wire.ensure_open()?;
        let pages: Vec<_> = self
            .targets
            .0
            .lock()
            .map_err(|_| internal("target lock poisoned"))?
            .snapshot()
            .into_iter()
            .filter(|target| target.kind == "page")
            .collect();
        if pages.is_empty() {
            return Ok(String::new());
        }
        if pages.len() != 1 {
            return Err(internal("private probe has unexpected pages"));
        }
        let target = &pages[0];
        let result = evaluate_before(
            &self.wire,
            &target.session,
            "document.getElementById('brows-fingerprint-probe')?.textContent || ''",
            deadline,
        )?;
        if !self
            .targets
            .0
            .lock()
            .map_err(|_| internal("target lock poisoned"))?
            .target_current(target)
        {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "private probe page changed during capture",
            ));
        }
        result
            .as_str()
            .filter(|value| value.len() <= 8192)
            .map(str::to_owned)
            .ok_or_else(|| internal("private first-script proof exceeds its bound"))
    }

    pub(super) fn observe(&self, session_id: &SessionId) -> PortResult<SessionObservation> {
        self.wire.ensure_open()?;
        let deadline = Instant::now() + TIMEOUT;
        let targets: Vec<_> = self
            .targets
            .0
            .lock()
            .map_err(|_| internal("target lock poisoned"))?
            .snapshot();
        if targets.len() > 32 {
            return Err(internal("session observation exceeds target limit"));
        }
        let mut result = Vec::new();
        for target in targets {
            let observed = evaluate_before(&self.wire, &target.session, OBSERVE, deadline);
            if !self
                .targets
                .0
                .lock()
                .map_err(|_| internal("target lock poisoned"))?
                .target_current(&target)
            {
                continue;
            }
            let signals = observed?;
            let reading = TargetObservation {
                kind: target.kind.clone(),
                user_agent: string(&signals, "userAgent")?,
                platform: string(&signals, "platform")?,
                languages: signals["languages"]
                    .as_array()
                    .ok_or_else(|| internal("invalid language observation"))?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| internal("invalid language observation"))
                    })
                    .collect::<PortResult<_>>()?,
                hardware_concurrency: signals["hardwareConcurrency"]
                    .as_u64()
                    .ok_or_else(|| internal("invalid hardware observation"))?,
                device_memory: signals["deviceMemory"].to_string(),
                timezone: string(&signals, "timezone")?,
                client_hints_platform: signals["clientHintsPlatform"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned(),
            };
            result.push((target, reading));
        }
        let state = self
            .targets
            .0
            .lock()
            .map_err(|_| internal("target lock poisoned"))?;
        if Instant::now() >= deadline {
            return Err(observation_timeout());
        }
        self.wire.ensure_open()?;
        Ok(SessionObservation {
            session_id: session_id.clone(),
            observed_at_ms: now_ms(),
            targets: state.current_values(result),
        })
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.wire.fail();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
fn string(value: &Value, key: &str) -> PortResult<String> {
    value[key]
        .as_str()
        .filter(|s| s.len() <= 2048)
        .map(str::to_owned)
        .ok_or_else(|| internal("invalid session observation"))
}
fn target_execution_ended(error: &Value) -> bool {
    error["code"] == -32000
        && matches!(
            error["message"].as_str(),
            Some("Target crashed" | "Inspected target navigated or closed")
        )
}
fn evaluate(wire: &Wire, session: &str, expression: &str) -> PortResult<Value> {
    evaluate_before(wire, session, expression, Instant::now() + TIMEOUT)
}
fn observation_timeout() -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        "session observation deadline exceeded",
    )
}
fn evaluate_before(
    wire: &Wire,
    session: &str,
    expression: &str,
    deadline: Instant,
) -> PortResult<Value> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(observation_timeout)?;
    let timeout_ms = remaining.as_millis().clamp(1, 1500) as u64;
    let reply = wire.begin(
        "Runtime.evaluate",
        json!({"expression":expression,"returnByValue":true,"silent":true,"timeout":timeout_ms}),
        Some(session),
    )?;
    let result = wire.finish_until(reply, deadline)?;
    if result.get("exceptionDetails").is_some() {
        return Err(internal(
            "session observation could not read the requested APIs",
        ));
    }
    Ok(result["result"]["value"].clone())
}
fn auto_attach(root: bool) -> Value {
    // Browser owns pages and origin-wide workers. Recursive sessions own only
    // their frame/dedicated-worker children, preventing duplicate controllers
    // from racing to pause and resume the same Service Worker.
    let filter = if root {
        json!([{"type":"page"},{"type":"shared_worker"},{"type":"service_worker"}])
    } else {
        json!([{"type":"iframe"},{"type":"worker"}])
    };
    json!({"autoAttach":true,"waitForDebuggerOnStart":true,"flatten":true,"filter":filter})
}
fn configure(
    wire: &Wire,
    session: &str,
    kind: &str,
    policy: &SessionFingerprintConfig,
) -> PortResult<()> {
    wire.call("Target.setAutoAttach", auto_attach(false), Some(session))?;
    let (platform, platform_version) = match policy.persona.platform {
        PlatformKind::Windows => ("Windows", "10.0.0"),
        PlatformKind::MacOs => ("macOS", "10.15.7"),
        PlatformKind::Linux => ("Linux", ""),
    };
    let platform_string = match policy.persona.platform {
        PlatformKind::Windows => "Win32",
        PlatformKind::MacOs => "MacIntel",
        PlatformKind::Linux => "Linux x86_64",
    };
    let version = policy
        .user_agent
        .split("Chrome/")
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .ok_or_else(|| invalid_data("missing Chromium version in policy"))?;
    let major = version.split('.').next().unwrap_or_default();
    let actual = wire
        .version
        .lock()
        .map_err(|_| internal("version lock poisoned"))?
        .clone();
    if actual.split('.').next() != Some(major) {
        return Err(internal("registered and running browser versions differ"));
    }
    wire.call("Emulation.setUserAgentOverride",json!({"userAgent":policy.user_agent,"acceptLanguage":policy.languages.join(","),"platform":platform_string,
  "userAgentMetadata":{"brands":[{"brand":"Chromium","version":major},{"brand":"Google Chrome","version":major}],"fullVersionList":[{"brand":"Chromium","version":actual},{"brand":"Google Chrome","version":actual}],"fullVersion":actual,"platform":platform,"platformVersion":platform_version,"architecture":"x86","bitness":"64","model":"","mobile":false,"wow64":false}}),Some(session))?;
    let is_page = kind == "page" || kind == "iframe";
    if let Some(timezone) = &policy.timezone {
        let result = wire.call(
            "Emulation.setTimezoneOverride",
            json!({"timezoneId":timezone}),
            Some(session),
        );
        if let Err(error) = result {
            if is_page && error.kind != PortErrorKind::AlreadyExists {
                return Err(error);
            }
            let verify = format!("Intl.DateTimeFormat().resolvedOptions().timeZone===Intl.DateTimeFormat('en',{{timeZone:{}}}).resolvedOptions().timeZone", json!(timezone));
            if !is_page && evaluate(wire, session, &verify)? != true {
                return Err(internal("native timezone is unavailable for this target"));
            }
        }
    }
    {
        let memory = policy.persona.device_memory.reported_f64();
        let values = json!({"hardwareConcurrency":policy.persona.hardware_concurrency,"deviceMemory":memory,"languages":policy.languages,"language":policy.languages[0],"platform":platform_string});
        let hardware_script=format!("(()=>{{const values={values};const p=Object.getPrototypeOf(navigator);for(const [key,value] of Object.entries(values)){{const d=Object.getOwnPropertyDescriptor(p,key);if(!d?.get&&key==='deviceMemory')continue;if(!d?.get||!d.configurable)throw new Error('Unsupported target property');const original=d.get;const reported=Array.isArray(value)?Object.freeze([...value]):value;Object.defineProperty(p,key,{{...d,get(){{original.call(this);return reported;}}}});}}return navigator.hardwareConcurrency===values.hardwareConcurrency&&(typeof navigator.deviceMemory==='undefined'||navigator.deviceMemory===values.deviceMemory);}})()");
        let rendering_script = brows_application::render_session_fingerprint(policy, &actual)?;
        let script = format!("{hardware_script};\n{rendering_script}");
        if is_page {
            wire.call("Page.enable", json!({}), Some(session))?;
            wire.call(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({"source":script,"runImmediately":true}),
                Some(session),
            )?;
        }
        if !is_page && evaluate(wire, session, &script)? != true {
            return Err(internal("target hardware policy did not apply"));
        }
    }
    Ok(())
}
const OBSERVE:&str="(()=>({userAgent:navigator.userAgent,platform:navigator.platform,languages:Array.from(navigator.languages),hardwareConcurrency:navigator.hardwareConcurrency,deviceMemory:navigator.deviceMemory,timezone:Intl.DateTimeFormat().resolvedOptions().timeZone,clientHintsPlatform:navigator.userAgentData?.platform??''}))()";

#[cfg(test)]
mod tests {
    use super::*;
    mod browser_fixture {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/test_support/controlled_browser.rs"
        ));
    }

    #[test]
    fn only_explicit_protocol_lifecycle_errors_mark_execution_ended() {
        for (code, message, expected) in [
            (-32000, "Target crashed", true),
            (-32000, "Inspected target navigated or closed", true),
            (-32602, "Target crashed", false),
            (-32602, "Inspected target navigated or closed", false),
            (-32000, "policy rejected", false),
            (-32000, "Target crashed unexpectedly", false),
        ] {
            let error = json!({"code":code,"message":message});
            assert_eq!(target_execution_ended(&error), expected);
        }
    }

    #[test]
    fn abandoned_replies_release_the_bounded_pending_budget() {
        let pending = Arc::new(Mutex::new(BTreeMap::new()));
        let (send, receive) = mpsc::channel();
        pending.lock().unwrap().insert(1, send);
        let reply = PendingReply {
            id: 1,
            method: "Runtime.enable".to_owned(),
            receive,
            session: Some("s".to_owned()),
            pending: pending.clone(),
        };
        drop(reply);
        assert!(pending.lock().unwrap().is_empty());
    }

    #[test]
    fn expired_shared_deadline_rejects_even_an_already_queued_reply() {
        let deadline = Instant::now();
        let pending = Arc::new(Mutex::new(BTreeMap::new()));
        let (send, receive) = mpsc::channel();
        send.send(json!({"result":{"value":true}})).unwrap();
        pending.lock().unwrap().insert(1, send);
        let reply = PendingReply {
            id: 1,
            method: "Runtime.evaluate".to_owned(),
            receive,
            session: Some("s".to_owned()),
            pending: pending.clone(),
        };
        assert!(matches!(
            reply.receive_until(deadline),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(reply);
        assert!(pending.lock().unwrap().is_empty());
    }

    #[derive(Debug)]
    struct AcceptanceBrowser {
        controller: Controller,
        root: std::path::PathBuf,
        webgl: Value,
        user_agent: String,
    }

    impl Drop for AcceptanceBrowser {
        fn drop(&mut self) {
            let _ = self.controller.kill();
            let _ = self.controller.wait();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    impl AcceptanceBrowser {
        fn assert_signals(&self, value: &Value) {
            assert_eq!(value["userAgent"].as_str(), Some(self.user_agent.as_str()));
            assert_eq!(value["platform"], "Win32");
            assert_eq!(value["hardwareConcurrency"], 3);
            assert_eq!(value["deviceMemory"], 0.5);
            assert_eq!(value["languages"], json!(["en-US", "en"]));
            assert_eq!(value["timezone"], "Asia/Tokyo");
            assert_eq!(value["offsetWinter"], -540);
            assert_eq!(value["offsetSummer"], -540);
            assert_eq!(value["canvas"], value["repeat"]);
            if !value["webgl"].is_null() {
                assert_eq!(value["webgl"], self.webgl);
            }
        }
        fn write_report(&self, name: &str, observations: Value) {
            let version = self.controller.wire.version.lock().unwrap().clone();
            let report = json!({"scope":"isolated signed Edge private controller", "runtimeVersion":version,
                "assertionsPassed":true,"complete":false,"observations":observations,
                "uncovered":["public DNS/ICE", "fonts/GPU/network stack", "other lifecycle scenarios"]});
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../target/fingerprint-{name}-acceptance.json"));
            std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        }
    }

    fn acceptance_browser(
        fixture: &browser_fixture::ControlFixture,
        route: &str,
    ) -> AcceptanceBrowser {
        use brows_domain::{DeviceMemoryGb, ScreenConfig, SeedableNoisePersona, WebGLProfile};
        let executable = std::path::PathBuf::from(
            std::env::var_os("BROWS_ACCEPTANCE_BROWSER").expect("explicit acceptance browser"),
        );
        assert_eq!(
            crate::verify_authenticode_publisher(&executable, &["Microsoft Corporation"]).unwrap(),
            "Microsoft Corporation"
        );
        let version = std::env::var("BROWS_ACCEPTANCE_VERSION").expect("verified binary version");
        static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "brows-private-acceptance-{}-{}-{}",
            std::process::id(),
            now_ms(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let persona = SeedableNoisePersona {
            seed: "worker-lifecycle".to_owned(),
            platform: PlatformKind::Windows,
            hardware_concurrency: 3,
            device_memory: DeviceMemoryGb::Half,
            touch_points: 0,
            screen: ScreenConfig {
                width: 1920,
                height: 1080,
                color_depth: 24,
                device_scale_factor_hundredths: 100,
            },
            webgl: WebGLProfile {
                vendor: String::new(),
                unmasked_renderer: String::new(),
            },
            spoof_canvas: true,
            spoof_audio: false,
        };
        let fp =
            brows_application::persona_fingerprint::PersonaFingerprint::derive(&persona, &version)
                .unwrap();
        let expected_webgl = json!({"vendor":fp.webgl_vendor,"renderer":fp.webgl_renderer});
        let policy = SessionFingerprintConfig {
            user_agent: fp.user_agent,
            persona,
            languages: vec!["en-US".to_owned(), "en".to_owned()],
            timezone: Some("Asia/Tokyo".to_owned()),
        };
        let user_agent = policy.user_agent.clone();
        let args = vec![
            OsString::from(format!("--user-data-dir={}", root.display())),
            OsString::from("--no-first-run"),
            OsString::from("--no-default-browser-check"),
            OsString::from("--no-startup-window"),
            OsString::from(format!("--user-agent={user_agent}")),
            OsString::from("--lang=en-US"),
        ];
        let controller = Controller::start(
            &executable,
            &args,
            policy,
            &[format!("{}/{route}/page", fixture.url)],
        )
        .unwrap();
        AcceptanceBrowser {
            controller,
            root,
            webgl: expected_webgl,
            user_agent,
        }
    }

    #[test]
    #[ignore = "requires an explicitly supplied signed Edge and uses isolated data"]
    fn private_worker_lifecycle_acceptance() {
        let fixture = browser_fixture::ControlFixture::start(false);
        let browser = acceptance_browser(&fixture, "life");
        let controller = &browser.controller;
        let expected_webgl = browser.webgl.clone();
        let initial = fixture.wait_proofs("/life/");
        assert!(
            controller.try_wait().unwrap().is_none(),
            "controller exited: {:?}",
            controller.wire.policy_failure.lock().unwrap()
        );
        assert_eq!(
            initial
                .iter()
                .map(|(_, _, first)| first["kind"].as_str().unwrap())
                .collect::<BTreeSet<_>>()
                .len(),
            6
        );
        let first = initial
            .iter()
            .find(|(_, _, first)| first["kind"] == "service_worker")
            .unwrap()
            .2
            .clone();
        if !first["webgl"].is_null() {
            assert_eq!(first["webgl"], expected_webgl);
        }
        for (_, _, value) in &initial {
            assert_eq!(value["hardwareConcurrency"], 3, "{} CPU", value["kind"]);
            assert_eq!(
                value["timezone"], "Asia/Tokyo",
                "{} timezone",
                value["kind"]
            );
            assert_eq!(value["canvas"], first["canvas"], "{} Canvas", value["kind"]);
            assert_eq!(value["repeat"], value["canvas"]);
            if !value["webgl"].is_null() {
                assert_eq!(value["webgl"], expected_webgl);
            }
        }
        let page = controller
            .targets
            .0
            .lock()
            .unwrap()
            .snapshot()
            .into_iter()
            .find(|target| target.kind == "page")
            .unwrap();
        controller
            .wire
            .call("ServiceWorker.enable", json!({}), Some(&page.session))
            .unwrap();
        controller
            .wire
            .call(
                "ServiceWorker.stopAllWorkers",
                json!({}),
                Some(&page.session),
            )
            .unwrap();
        let deadline = Instant::now() + TIMEOUT;
        while controller
            .targets
            .0
            .lock()
            .unwrap()
            .snapshot()
            .iter()
            .any(|target| target.kind == "service_worker")
        {
            assert!(
                Instant::now() < deadline,
                "stopped Worker must leave observation"
            );
            thread::sleep(Duration::from_millis(50));
        }
        let session_id = SessionId::new("lifecycle").unwrap();
        assert!(!controller
            .observe(&session_id)
            .unwrap()
            .targets
            .iter()
            .any(|target| target.kind == "service_worker"));
        evaluate(&controller.wire,&page.session,"navigator.serviceWorker.ready.then(registration=>registration.active.postMessage('restart-proof'));true").unwrap();
        let restarted = fixture.wait_kind_count("/life/", "service_worker", 2);
        assert!(
            restarted.len() >= 2,
            "restart must run its first script again"
        );
        fixture.bump_service_version();
        let updated = fixture.wait_kind_count("/life/", "service_worker", 3);
        assert!(
            updated.len() >= 3,
            "updated Worker must run its first script"
        );
        assert_eq!(updated.last().unwrap().2["fixtureVersion"], 1);
        for (_, _, value) in updated {
            assert_eq!(value["hardwareConcurrency"], 3);
            assert_eq!(value["timezone"], "Asia/Tokyo");
            assert_eq!(value["canvas"], first["canvas"]);
            assert_eq!(value["repeat"], value["canvas"]);
            assert_eq!(value["webgl"], first["webgl"]);
        }
        assert!(controller
            .observe(&session_id)
            .unwrap()
            .targets
            .iter()
            .any(|target| target.kind == "service_worker"));
    }

    #[test]
    #[ignore = "requires an explicitly supplied signed Edge and uses isolated data"]
    fn private_module_worker_acceptance() {
        let fixture = browser_fixture::ControlFixture::start(true);
        let browser = acceptance_browser(&fixture, "modules");
        let proofs = fixture.wait_proofs("/modules/");
        assert!(browser.controller.try_wait().unwrap().is_none());
        let kinds: BTreeSet<_> = proofs
            .iter()
            .map(|(_, _, value)| value["kind"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds.len(),
            9,
            "all classic and module contexts must report"
        );
        let page = &proofs
            .iter()
            .find(|(_, _, value)| value["kind"] == "page")
            .unwrap()
            .2;
        assert_ne!(page["canvas"], json!("a42367ff".repeat(256)));
        for (_, _, first) in &proofs {
            browser.assert_signals(first);
            assert_eq!(first["canvas"], page["canvas"]);
            assert_eq!(first["hints"], page["hints"]);
        }
        for (module, classic) in [
            ("module_worker", "worker"),
            ("module_shared_worker", "shared_worker"),
            ("module_service_worker", "service_worker"),
        ] {
            let sample = &proofs
                .iter()
                .find(|(_, _, value)| value["kind"] == module)
                .unwrap()
                .2;
            let baseline = &proofs
                .iter()
                .find(|(_, _, value)| value["kind"] == classic)
                .unwrap()
                .2;
            assert_eq!(sample["capturePhase"], "module_dependency");
            if !baseline["webgl"].is_null() {
                assert_eq!(sample["webgl"], baseline["webgl"]);
            }
        }
        browser.write_report(
            "module-worker",
            json!(proofs.iter().map(|(_, _, value)| value).collect::<Vec<_>>()),
        );
    }

    #[test]
    #[ignore = "crashes only an explicitly launched isolated signed Edge renderer"]
    fn private_renderer_crash_acceptance() {
        let fixture = browser_fixture::ControlFixture::start(false);
        let browser = acceptance_browser(&fixture, "crash");
        let controller = &browser.controller;
        let initial = fixture.wait_proofs("/crash/");
        assert_eq!(
            initial
                .iter()
                .map(|(_, _, value)| value["kind"].as_str().unwrap())
                .collect::<BTreeSet<_>>()
                .len(),
            6
        );
        let first = &initial
            .iter()
            .find(|(_, _, value)| value["kind"] == "page")
            .unwrap()
            .2;
        browser.assert_signals(first);
        let page = controller
            .targets
            .0
            .lock()
            .unwrap()
            .snapshot()
            .into_iter()
            .find(|target| target.kind == "page")
            .unwrap();
        let deadline = Instant::now() + TIMEOUT;
        while evaluate(&controller.wire, &page.session, "document.readyState").unwrap()
            != "complete"
        {
            assert!(
                Instant::now() < deadline,
                "owned page must finish navigation before crash"
            );
            thread::sleep(Duration::from_millis(50));
        }
        if let Err(error) = controller
            .wire
            .call("Page.crash", json!({}), Some(&page.session))
        {
            assert_eq!(
                error.kind,
                PortErrorKind::Unavailable,
                "unexpected crash command error: {error:?}"
            );
        }
        let deadline = Instant::now() + TIMEOUT;
        while controller
            .targets
            .0
            .lock()
            .unwrap()
            .snapshot()
            .iter()
            .any(|target| target.session == page.session)
        {
            assert!(
                Instant::now() < deadline,
                "crashed page must leave observation"
            );
            thread::sleep(Duration::from_millis(50));
        }
        assert!(
            controller.try_wait().unwrap().is_none(),
            "renderer crash must not close browser"
        );
        let session_id = SessionId::new("renderer-crash").unwrap();
        assert!(!controller
            .observe(&session_id)
            .unwrap()
            .targets
            .iter()
            .any(|target| target.kind == "page"));
        controller
            .wire
            .call(
                "Page.reload",
                json!({"ignoreCache":true}),
                Some(&page.session),
            )
            .unwrap();
        let reloaded = fixture.wait_kind_count("/crash/", "page", 2);
        assert!(
            reloaded.len() >= 2,
            "reload must execute a new first script"
        );
        let recovered = &reloaded.last().unwrap().2;
        browser.assert_signals(recovered);
        assert_eq!(recovered["canvas"], first["canvas"]);
        assert_eq!(recovered["webgl"], first["webgl"]);
        let deadline = Instant::now() + TIMEOUT;
        while !controller
            .targets
            .0
            .lock()
            .unwrap()
            .snapshot()
            .iter()
            .any(|target| target.kind == "page")
        {
            assert!(
                Instant::now() < deadline,
                "recovered page must be configured again"
            );
            thread::sleep(Duration::from_millis(50));
        }
        let observed = controller.observe(&session_id).unwrap();
        assert!(observed.targets.iter().any(|target| target.kind == "page"
            && target.hardware_concurrency == 3
            && target.timezone == "Asia/Tokyo"));
        browser.write_report("renderer-crash", json!({"before":first,"after":recovered,"crashedPageRemoved":true,"browserSurvived":true}));
    }
}
