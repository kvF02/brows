//! Session-scoped control and observation over private inherited pipes.
//! No generic command/evaluate surface is exposed through application ports.
use super::{
    internal, invalid_data,
    navigation::{navigation_reply_acknowledged, NavigationStart},
    now_ms,
    target_registry::{Target, TargetJob, Targets, MAX_TARGETS},
    win_pipe,
};
use brows_application::{
    PortError, PortErrorKind, PortResult, SessionObservation, TargetObservation,
};
use brows_domain::{PlatformKind, ScreenConfig, SessionFingerprintConfig, SessionId};
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
    #[cfg(any(test, debug_assertions))]
    trace: Mutex<std::collections::VecDeque<String>>,
    #[cfg(any(test, debug_assertions))]
    trace_started: Instant,
    #[cfg(any(test, debug_assertions))]
    panic_traced: AtomicBool,
    process: Arc<win_pipe::Process>,
    next: AtomicU64,
    closing: AtomicBool,
    orderly_shutdown: AtomicBool,
    version: Mutex<String>,
    policy_failure: Mutex<Option<String>>,
    navigation: Mutex<Option<NavigationStart>>,
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
    #[cfg(any(test, debug_assertions))]
    fn trace(&self, phase: &str, method: &str, session: Option<&str>) {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        session.hash(&mut hash);
        if let Ok(mut trace) = self.trace.lock() {
            if trace.len() == 128 {
                trace.pop_front();
            }
            trace.push_back(format!(
                "{}ms {phase} {method} target={:x}",
                self.trace_started.elapsed().as_millis(),
                hash.finish()
            ));
        }
    }
    #[cfg(any(test, debug_assertions))]
    fn trace_failure(&self) {
        if let Ok(trace) = self.trace.lock() {
            eprintln!(
                "bounded control trace (methods and redacted document state):\n{}",
                trace.iter().cloned().collect::<Vec<_>>().join("\n")
            );
        }
        eprintln!(
            "control policy failure: {:?}; process exit: {:?}",
            self.policy_failure.lock().ok().as_deref(),
            self.process.try_wait().ok()
        );
    }
    fn transport_closed(&self) {
        #[cfg(any(test, debug_assertions))]
        self.trace("transport", "closed", None);
        if !self.orderly_shutdown.load(Ordering::Acquire) {
            self.fail();
            return;
        }
        // Browser.close can disconnect CDP before the final storage process
        // exits. Keep the owned Job alive until bounded shutdown drains it.
        self.closing.store(true, Ordering::Release);
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
    }
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
        #[cfg(any(test, debug_assertions))]
        self.trace("send", &format!("{id} {method}"), session);
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
        let value = reply
            .receive_until(deadline)
            .map_err(|error| self.reply_error(&reply, error))?;
        self.complete_reply(&reply, value)
    }
    fn reply_error(&self, reply: &PendingReply, error: mpsc::RecvTimeoutError) -> PortError {
        let method = &reply.method;
        #[cfg(any(test, debug_assertions))]
        self.trace_failure();
        let failure = self
            .policy_failure
            .lock()
            .ok()
            .and_then(|reason| reason.clone());
        if reply.session.is_none() {
            self.fail();
        }
        PortError::new(
            PortErrorKind::Unavailable,
            reply_failure_message(method, error, failure.as_deref()),
        )
    }
    fn complete_reply(&self, reply: &PendingReply, value: Value) -> PortResult<Value> {
        let method = &reply.method;
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
    fn prepare_startup_document(
        &self,
        session: &str,
        policy: &SessionFingerprintConfig,
    ) -> PortResult<String> {
        // A policy-ready target and its frame tree can exist before its initial
        // renderer document is ready. Complete an owned, network-free document
        // first; never use the external site's response as this barrier.
        let deadline = Instant::now() + TIMEOUT;
        let tree = self.finish_until(
            self.begin("Page.getFrameTree", json!({}), Some(session))?,
            deadline,
        )?;
        let frame = tree["frameTree"]["frame"]["id"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 128)
            .ok_or_else(|| internal("browser omitted the startup frame identity"))?;
        let result = self.finish_until(
            self.begin(
                "Page.navigate",
                json!({"url":"about:blank","frameId":frame,"transitionType":"reload"}),
                Some(session),
            )?,
            deadline,
        )?;
        if !navigation_reply_acknowledged(&result, frame)
            || !result["errorText"].is_null()
            || result["isDownload"] == true
        {
            return Err(internal(
                "browser could not initialize the startup document",
            ));
        }
        // Fixed internal read in our blank document, after the policy is
        // installed and the target is resumed. No caller-supplied expression.
        let native_check = native_navigator_check(policy);
        let ready = self.finish_until(
            self.begin(
                "Runtime.evaluate",
                json!({
                    "expression":format!("new Promise(resolve => {{ const ready = () => resolve(location.href === 'about:blank' && document.readyState === 'complete' && {native_check}); if (document.readyState === 'complete') ready(); else addEventListener('load', ready, {{once:true}}); }})"),
                    "awaitPromise":true,"returnByValue":true,"silent":true,"timeout":1500
                }),
                Some(session),
            )?,
            deadline,
        )?;
        if ready.get("exceptionDetails").is_some() || ready["result"]["value"] != true {
            return Err(internal(
                "browser startup document or native Navigator policy is not ready",
            ));
        }
        #[cfg(any(test, debug_assertions))]
        self.trace("document-ready", "blank", Some(session));
        Ok(frame.to_owned())
    }
    fn start_navigation(&self, session: &str, frame: &str, url: &str) -> PortResult<()> {
        {
            let mut navigation = self
                .navigation
                .lock()
                .map_err(|_| internal("navigation lock poisoned"))?;
            if navigation.is_some() {
                return Err(internal("startup navigation already pending"));
            }
            *navigation = Some(
                NavigationStart::new(session, frame, url)
                    .ok_or_else(|| invalid_data("invalid startup navigation"))?,
            );
        }
        let outcome = (|| {
            let reply = self.begin(
                "Page.navigate",
                json!({"url":url,"frameId":frame}),
                Some(session),
            )?;
            let deadline = Instant::now() + TIMEOUT;
            loop {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    return Err(self.reply_error(&reply, mpsc::RecvTimeoutError::Timeout));
                };
                match reply
                    .receive
                    .recv_timeout(remaining.min(Duration::from_millis(25)))
                {
                    Ok(value) => {
                        let result = self.complete_reply(&reply, value)?;
                        if !navigation_reply_acknowledged(&result, frame) {
                            return Err(internal("browser omitted the requested navigation frame"));
                        }
                        #[cfg(any(test, debug_assertions))]
                        if let Some(code) = super::navigation::document_result_code(&result) {
                            eprintln!("startup navigation document result: {code}");
                        }
                        return Ok(());
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(self.reply_error(&reply, mpsc::RecvTimeoutError::Disconnected))
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                self.ensure_open()?;
                if Instant::now() >= deadline {
                    return Err(self.reply_error(&reply, mpsc::RecvTimeoutError::Timeout));
                }
                if self
                    .navigation
                    .lock()
                    .map_err(|_| internal("navigation lock poisoned"))?
                    .as_ref()
                    .is_some_and(NavigationStart::started)
                {
                    // The matching main-frame request has started. Network load
                    // and its eventual navigation result belong to the page,
                    // not the control-policy acknowledgement budget.
                    return Ok(());
                }
            }
        })();
        self.navigation
            .lock()
            .map_err(|_| internal("navigation lock poisoned"))?
            .take();
        outcome
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
    #[cfg(any(test, debug_assertions))]
    fn trace_failure_state(&self, expected_url: Option<&str>) {
        // A production-composition test uses this crate as a dependency, where
        // cfg(test) is false. Keep evidence in debug builds without enabling
        // diagnostics or additional commands in the release application.
        if self.wire.panic_traced.swap(true, Ordering::AcqRel) {
            return;
        }
        self.wire.trace_failure();
        let pages = self
            .targets
            .0
            .lock()
            .map(|state| state.snapshot())
            .unwrap_or_default();
        let deadline = Instant::now() + Duration::from_millis(750);
        for target in pages
            .into_iter()
            .filter(|target| target.kind == "page")
            .take(2)
        {
            if Instant::now() >= deadline {
                break;
            }
            let state = self
                .wire
                .begin("Page.getFrameTree", json!({}), Some(&target.session))
                .and_then(|reply| self.wire.finish_until(reply, deadline));
            if let Ok(state) = state {
                let frame = &state["frameTree"]["frame"];
                eprintln!(
                    "failed page state: kind={} requested={:?} unreachable={}",
                    super::navigation::document_url_kind(frame["url"].as_str().unwrap_or("")),
                    expected_url.map(|url| frame["url"].as_str() == Some(url)),
                    frame["unreachableUrl"]
                        .as_str()
                        .is_some_and(|url| !url.is_empty())
                );
            }
        }
    }

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
            #[cfg(any(test, debug_assertions))]
            trace: Mutex::new(std::collections::VecDeque::new()),
            #[cfg(any(test, debug_assertions))]
            trace_started: Instant::now(),
            #[cfg(any(test, debug_assertions))]
            panic_traced: AtomicBool::new(false),
            process: Arc::new(process),
            next: AtomicU64::new(1),
            closing: AtomicBool::new(false),
            orderly_shutdown: AtomicBool::new(false),
            version: Mutex::new(String::new()),
            policy_failure: Mutex::new(None),
            navigation: Mutex::new(None),
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
                            w.transport_closed();
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
                        #[cfg(any(test, debug_assertions))]
                        if value["method"] == "Page.frameNavigated" {
                            w.trace(
                                "document-committed",
                                super::navigation::document_url_kind(
                                    value["params"]["frame"]["url"].as_str().unwrap_or(""),
                                ),
                                value["sessionId"].as_str(),
                            );
                        }
                        if let Some(id) = value["id"].as_u64() {
                            #[cfg(any(test, debug_assertions))]
                            w.trace("reply", &id.to_string(), value["sessionId"].as_str());
                            #[cfg(any(test, debug_assertions))]
                            if let Some(code) =
                                super::navigation::document_result_code(&value["result"])
                            {
                                // start_navigation may already have accepted a
                                // correlated start event and released its waiter.
                                w.trace("document-result", code, value["sessionId"].as_str());
                            }
                            if let Ok(mut pending) = w.pending.lock() {
                                if let Some(sender) = pending.remove(&id) {
                                    let _ = sender.send(value);
                                }
                            }
                        } else if value["method"] == "Page.frameStartedNavigating" {
                            #[cfg(any(test, debug_assertions))]
                            w.trace(
                                "document-started",
                                super::navigation::document_url_kind(
                                    value["params"]["url"].as_str().unwrap_or(""),
                                ),
                                value["sessionId"].as_str(),
                            );
                            if let Ok(mut navigation) = w.navigation.lock() {
                                if let Some(navigation) = navigation.as_mut() {
                                    navigation.observe(
                                        value["sessionId"].as_str().unwrap_or(""),
                                        value["params"]["frameId"].as_str().unwrap_or(""),
                                        value["params"]["url"].as_str().unwrap_or(""),
                                    );
                                }
                            }
                        } else if value["method"] == "Debugger.paused" {
                            #[cfg(any(test, debug_assertions))]
                            w.trace("event", "Debugger.paused", value["sessionId"].as_str());
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
                                    | "ServiceWorker.workerVersionUpdated"
                                    | "Inspector.targetCrashed"
                                    | "Inspector.targetReloadedAfterCrash"
                            )
                        ) {
                            #[cfg(any(test, debug_assertions))]
                            w.trace(
                                "event",
                                value["method"].as_str().unwrap_or(""),
                                value["params"]["sessionId"]
                                    .as_str()
                                    .or(value["sessionId"].as_str()),
                            );
                            if events.try_send(value).is_err() {
                                break 'read;
                            }
                        }
                    } else {
                        frame.push(*byte);
                        if frame.len() > MAX_FRAME {
                            break 'read;
                        }
                    }
                }
            }
            w.transport_closed();
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
                    #[cfg(any(test, debug_assertions))]
                    w.trace("policy-begin", &job.target.kind, Some(session));
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
                                // Disabling the private debugger releases its pause.
                                // A separate resume lets the worker exit before disable can reply.
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
                        #[cfg(any(test, debug_assertions))]
                        w.trace("policy-error", &job.target.kind, Some(session));
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
                        #[cfg(any(test, debug_assertions))]
                        w.trace("policy-ready", &job.target.kind, Some(session));
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
                if event["method"] == "ServiceWorker.workerVersionUpdated" {
                    let outcome: PortResult<Vec<String>> = (|| {
                        let versions = event["params"]["versions"]
                            .as_array()
                            .filter(|versions| versions.len() <= MAX_TARGETS * 4)
                            .ok_or_else(|| internal("invalid Service Worker version update"))?;
                        let mut state = t.0.lock().map_err(|_| internal("target lock poisoned"))?;
                        let mut retired = Vec::new();
                        for version in versions {
                            let id = version["versionId"]
                                .as_str()
                                .ok_or_else(|| internal("missing Service Worker version"))?;
                            let status = version["status"]
                                .as_str()
                                .ok_or_else(|| internal("missing Service Worker status"))?;
                            let target = version["targetId"].as_str().filter(|id| !id.is_empty());
                            retired.extend(state.worker_version(
                                id,
                                target,
                                status == "redundant",
                            )?);
                        }
                        Ok(retired)
                    })();
                    match outcome {
                        Ok(retired) => {
                            if let Ok(mut paused) = w.paused.lock() {
                                for session in retired {
                                    paused.remove(&session);
                                }
                            }
                        }
                        Err(_) => {
                            w.fail();
                            break;
                        }
                    }
                    t.1.notify_all();
                    continue;
                }
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
            let frame = controller
                .wire
                .prepare_startup_document(&target.session, &policy)?;
            if url != "about:blank" {
                controller
                    .wire
                    .start_navigation(&target.session, &frame, &url)?;
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
        #[cfg(any(test, debug_assertions))]
        if std::thread::panicking() {
            self.trace_failure_state(None);
        }
        self.wire.fail();
        Ok(())
    }
    pub(super) fn request_close(&self) -> std::io::Result<bool> {
        self.wire.orderly_shutdown.store(true, Ordering::Release);
        match self.wire.begin("Browser.close", json!({}), None) {
            Ok(_) => Ok(true),
            Err(_) => {
                self.wire.orderly_shutdown.store(false, Ordering::Release);
                Err(std::io::Error::other("browser close request unavailable"))
            }
        }
    }
    pub(super) fn tree_exited(&self) -> std::io::Result<bool> {
        self.wire.process.tree_exited()
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
fn reply_failure_message(
    method: &str,
    error: mpsc::RecvTimeoutError,
    policy: Option<&str>,
) -> String {
    if let Some(policy) = policy {
        return policy.to_owned();
    }
    let condition = match error {
        mpsc::RecvTimeoutError::Timeout => "response timeout",
        mpsc::RecvTimeoutError::Disconnected => "disconnected while waiting",
    };
    format!("session control {condition}: {method}")
}
impl Drop for Controller {
    fn drop(&mut self) {
        #[cfg(any(test, debug_assertions))]
        if std::thread::panicking() {
            self.trace_failure_state(None);
        }
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
    let read_deadline = deadline.min(Instant::now() + Duration::from_millis(1500));
    let result = wire.finish_until(reply, read_deadline)?;
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
    if kind == "page" || kind == "iframe" {
        wire.call("Inspector.enable", json!({}), Some(session))?;
    }
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
    wire.call(
        "Emulation.setHardwareConcurrencyOverride",
        json!({"hardwareConcurrency":policy.persona.hardware_concurrency}),
        Some(session),
    )?;
    let is_page = kind == "page" || kind == "iframe";
    if kind == "page" {
        configure_native_screen(wire, session, &policy.persona.screen)?;
    }
    if is_page {
        let points = policy.persona.touch_points;
        let mut touch = json!({"enabled":points>0});
        if points > 0 {
            touch["maxTouchPoints"] = json!(points);
        }
        wire.call("Emulation.setTouchEmulationEnabled", touch, Some(session))?;
    }
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
        let rendering = brows_application::render_session_fingerprint(policy, &actual)?;
        let script = format!("{rendering}\n{}", native_navigator_check(policy));
        if is_page {
            wire.call("Page.enable", json!({}), Some(session))?;
            if kind == "page" {
                wire.call("ServiceWorker.enable", json!({}), Some(session))?;
            }
            wire.call(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({"source":script,"runImmediately":true}),
                Some(session),
            )?;
        }
        if !is_page && evaluate(wire, session, &script)? != true {
            return Err(internal("target native browser policy did not apply"));
        }
    }
    Ok(())
}
fn configure_native_screen(wire: &Wire, session: &str, screen: &ScreenConfig) -> PortResult<()> {
    // Metrics overrides are only accepted at the top target. Chromium carries
    // the resulting screen info to child frames. Zero view dimensions/DSF and
    // desktop mode preserve the real viewport, window decorations and DPR.
    let orientation = if screen.width < screen.height {
        "portraitPrimary"
    } else {
        "landscapePrimary"
    };
    wire.call(
        "Emulation.setDeviceMetricsOverride",
        json!({"width":0,"height":0,"deviceScaleFactor":0,"mobile":false,
            "dontSetVisibleSize":true,"screenWidth":screen.width,"screenHeight":screen.height,
            "screenOrientation":{"type":orientation,"angle":0}}),
        Some(session),
    )?;
    Ok(())
}

fn native_navigator_check(policy: &SessionFingerprintConfig) -> String {
    let platform = match policy.persona.platform {
        PlatformKind::Windows => "Win32",
        PlatformKind::MacOs => "MacIntel",
        PlatformKind::Linux => "Linux x86_64",
    };
    let expected = json!({"cpu":policy.persona.hardware_concurrency,"platform":platform,
        "languages":policy.languages,"touch":policy.persona.touch_points,
        "screenWidth":policy.persona.screen.width,"screenHeight":policy.persona.screen.height});
    format!(
        r#"(()=>{{
const expected={expected};
if(navigator.hardwareConcurrency!==expected.cpu||navigator.platform!==expected.platform||navigator.language!==expected.languages[0]||JSON.stringify(Array.from(navigator.languages))!==JSON.stringify(expected.languages))return false;
if(typeof document==='undefined')return true;
return navigator.maxTouchPoints===expected.touch&&screen.width===expected.screenWidth&&screen.height===expected.screenHeight&&screen.availWidth===expected.screenWidth&&screen.availHeight===expected.screenHeight&&matchMedia('(device-width: '+expected.screenWidth+'px) and (device-height: '+expected.screenHeight+'px)').matches;
}})()"#
    )
}
const OBSERVE:&str="(()=>({userAgent:navigator.userAgent,platform:navigator.platform,languages:Array.from(navigator.languages),hardwareConcurrency:navigator.hardwareConcurrency,deviceMemory:navigator.deviceMemory,timezone:Intl.DateTimeFormat().resolvedOptions().timeZone,clientHintsPlatform:navigator.userAgentData?.platform??''}))()";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reply_errors_preserve_policy_failure_and_distinguish_disconnect_from_timeout() {
        assert_eq!(
            reply_failure_message(
                "Page.navigate",
                mpsc::RecvTimeoutError::Disconnected,
                Some("worker policy failed")
            ),
            "worker policy failed"
        );
        assert_eq!(
            reply_failure_message("Page.navigate", mpsc::RecvTimeoutError::Disconnected, None),
            "session control disconnected while waiting: Page.navigate"
        );
        assert_eq!(
            reply_failure_message("Page.navigate", mpsc::RecvTimeoutError::Timeout, None),
            "session control response timeout: Page.navigate"
        );
    }
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
        startup_url: String,
        hardware_concurrency: u8,
        touch_points: u8,
        platform: String,
        languages: Vec<String>,
        screen: ScreenConfig,
    }

    impl Drop for AcceptanceBrowser {
        fn drop(&mut self) {
            if std::thread::panicking() {
                self.controller.trace_failure_state(Some(&self.startup_url));
            }
            let _ = self.controller.kill();
            let _ = self.controller.wait();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    impl AcceptanceBrowser {
        fn assert_signals(&self, value: &Value) {
            assert_eq!(value["userAgent"].as_str(), Some(self.user_agent.as_str()));
            assert_eq!(value["platform"], self.platform);
            assert_eq!(value["hardwareConcurrency"], self.hardware_concurrency);
            assert_eq!(value["deviceMemory"], 0.5);
            assert_eq!(value["languages"], json!(self.languages));
            assert_eq!(value["timezone"], "Asia/Tokyo");
            assert_eq!(value["offsetWinter"], -540);
            assert_eq!(value["offsetSummer"], -540);
            for name in ["hardwareConcurrency", "platform", "language", "languages"] {
                let native = &value["nativeNavigator"][name];
                // NavigatorBase::platform has no native Worker override in the
                // admitted Chromium. Cross-OS Worker platform remains a stated
                // script fallback; CPU and languages must still be native.
                let worker = value["touch"].is_null();
                let expected_native = !(name == "platform" && worker && self.platform != "Win32");
                assert_eq!(
                    native["nativeGetter"], expected_native,
                    "{} {name}",
                    value["kind"]
                );
                assert_eq!(
                    native["brandCheck"], true,
                    "{} {name} receiver",
                    value["kind"]
                );
            }
            assert_eq!(
                value["nativeNavigator"]["hardwareConcurrency"]["value"],
                self.hardware_concurrency
            );
            assert_eq!(value["nativeNavigator"]["platform"]["value"], self.platform);
            assert_eq!(
                value["nativeNavigator"]["language"]["value"],
                self.languages[0]
            );
            assert_eq!(
                value["nativeNavigator"]["languages"]["value"],
                json!(self.languages)
            );
            if !value["touch"].is_null() {
                let screen = &value["screen"];
                for (name, expected) in [
                    ("width", self.screen.width),
                    ("height", self.screen.height),
                    ("availWidth", self.screen.width),
                    ("availHeight", self.screen.height),
                ] {
                    assert_eq!(screen[name], expected, "{} screen {name}", value["kind"]);
                    assert_eq!(screen["getters"][name]["nativeGetter"], true);
                    assert_eq!(screen["getters"][name]["brandCheck"], true);
                    assert_eq!(screen["getters"][name]["value"], expected);
                }
                assert_eq!(screen["deviceMedia"], true);
                assert_eq!(screen["wrongDeviceMedia"], false);
                for name in ["colorDepth", "pixelDepth"] {
                    assert_eq!(screen["getters"][name]["value"], self.screen.color_depth);
                    assert_eq!(screen["getters"][name]["brandCheck"], true);
                }
                assert_eq!(value["touch"]["points"], self.touch_points);
                assert_eq!(
                    value["nativeNavigator"]["maxTouchPoints"]["nativeGetter"],
                    true
                );
                assert_eq!(
                    value["nativeNavigator"]["maxTouchPoints"]["brandCheck"],
                    true
                );
                if self.touch_points > 0 {
                    assert_eq!(value["touch"]["coarse"], true);
                    assert_eq!(value["touch"]["hoverNone"], true);
                    assert_eq!(value["touch"]["api"], true);
                }
            }
            assert_eq!(value["canvas"], value["repeat"]);
            if !value["webgl"].is_null() {
                assert_eq!(value["webgl"], self.webgl);
            }
            assert!(value["gpuPixels"].is_object());
            for kind in ["webgl", "webgl2"] {
                let sample = &value["gpuPixels"][kind];
                if sample.is_null() {
                    continue;
                }
                assert_eq!(sample["error"], Value::Null);
                for check in [
                    "repeatStable",
                    "cropConsistent",
                    "errorsPreserved",
                    "sourceUnchanged",
                ] {
                    assert_eq!(sample[check], true, "{} {kind} {check}", value["kind"]);
                }
                assert_eq!(
                    sample["pixels"], value["canvas"],
                    "{} {kind} readPixels/2D",
                    value["kind"]
                );
                assert_eq!(
                    decode_gpu_png(sample["png"].as_str().unwrap()),
                    sample["pixels"].as_str().unwrap(),
                    "{} {kind} exported PNG/readPixels",
                    value["kind"]
                );
                if kind == "webgl2" {
                    assert_pack_signals(&sample["pack"]);
                    let renderbuffer = &sample["renderbuffer"];
                    assert!(renderbuffer.is_object());
                    for check in [
                        "repeatStable",
                        "cropConsistent",
                        "sourceUnchanged",
                        "bindingsRestored",
                        "errorsPreserved",
                    ] {
                        assert_eq!(
                            renderbuffer[check], true,
                            "{} renderbuffer {check}",
                            value["kind"]
                        );
                    }
                    assert_eq!(renderbuffer["pixels"], renderbuffer["referencePixels"]);
                    assert_eq!(renderbuffer["pixels"].as_str().unwrap().len(), 9 * 7 * 8);
                    assert_pack_signals(&renderbuffer["pack"]);
                }
            }
        }
        fn write_report(&self, name: &str, observations: Value) {
            self.write_report_status(name, observations, true);
        }
        fn write_report_status(&self, name: &str, observations: Value, passed: bool) {
            let version = self.controller.wire.version.lock().unwrap().clone();
            let report = json!({"scope":"isolated signed Edge private controller", "runtimeVersion":version,
                "assertionsPassed":passed,"complete":false,"observations":observations,
                "uncovered":["public DNS/ICE", "fonts/GPU/network stack", "other lifecycle scenarios"]});
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../target/fingerprint-{name}-acceptance.json"));
            std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        }
    }

    fn assert_pack_signals(value: &Value) {
        assert_eq!(value["layoutsChecked"], 5);
        for check in ["pixelsMatch", "outsidePreserved", "errorsPreserved"] {
            assert_eq!(value[check], true, "pack {check}");
        }
    }

    fn acceptance_browser(
        fixture: &browser_fixture::ControlFixture,
        route: &str,
    ) -> AcceptanceBrowser {
        acceptance_browser_options(fixture, route, true, "worker-lifecycle")
    }
    fn acceptance_browser_options(
        fixture: &browser_fixture::ControlFixture,
        route: &str,
        canvas_noise: bool,
        seed: &str,
    ) -> AcceptanceBrowser {
        acceptance_browser_hardware(
            fixture,
            route,
            canvas_noise,
            seed,
            3,
            0,
            PlatformKind::Windows,
        )
    }
    fn acceptance_browser_hardware(
        fixture: &browser_fixture::ControlFixture,
        route: &str,
        canvas_noise: bool,
        seed: &str,
        hardware_concurrency: u8,
        touch_points: u8,
        platform: PlatformKind,
    ) -> AcceptanceBrowser {
        acceptance_browser_configured(fixture, route, canvas_noise, seed, |persona| {
            persona.hardware_concurrency = hardware_concurrency;
            persona.touch_points = touch_points;
            persona.platform = platform;
        })
    }

    fn acceptance_browser_configured(
        fixture: &browser_fixture::ControlFixture,
        route: &str,
        canvas_noise: bool,
        seed: &str,
        customize: impl FnOnce(&mut brows_domain::SeedableNoisePersona),
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
        let mut persona = SeedableNoisePersona {
            seed: seed.to_owned(),
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
            spoof_canvas: canvas_noise,
            spoof_audio: false,
        };
        customize(&mut persona);
        let screen = persona.screen;
        let platform = persona.platform;
        let hardware_concurrency = persona.hardware_concurrency;
        let touch_points = persona.touch_points;
        let fp =
            brows_application::persona_fingerprint::PersonaFingerprint::derive(&persona, &version)
                .unwrap();
        let expected_webgl = json!({"vendor":fp.webgl_vendor,"renderer":fp.webgl_renderer});
        let languages = if platform == PlatformKind::Windows {
            vec!["en-US".to_owned(), "en".to_owned()]
        } else {
            vec!["pl-PL".to_owned(), "pl".to_owned()]
        };
        let platform_name = match platform {
            PlatformKind::Windows => "Win32",
            PlatformKind::MacOs => "MacIntel",
            PlatformKind::Linux => "Linux x86_64",
        }
        .to_owned();
        let policy = SessionFingerprintConfig {
            user_agent: fp.user_agent,
            persona,
            languages: languages.clone(),
            timezone: Some("Asia/Tokyo".to_owned()),
        };
        let user_agent = policy.user_agent.clone();
        let args = vec![
            OsString::from(format!("--user-data-dir={}", root.display())),
            OsString::from("--no-first-run"),
            OsString::from("--no-default-browser-check"),
            OsString::from("--no-startup-window"),
            OsString::from("--window-size=760,600"),
            OsString::from(format!(
                "--force-device-scale-factor={}",
                f64::from(screen.device_scale_factor_hundredths) / 100.0
            )),
            OsString::from(format!("--user-agent={user_agent}")),
            OsString::from("--lang=en-US"),
        ];
        let startup_url = format!("{}/{route}/page", fixture.url);
        let controller = Controller::start(
            &executable,
            &args,
            policy,
            std::slice::from_ref(&startup_url),
        )
        .unwrap();
        AcceptanceBrowser {
            controller,
            root,
            webgl: expected_webgl,
            user_agent,
            startup_url,
            hardware_concurrency,
            touch_points,
            platform: platform_name,
            languages,
            screen,
        }
    }

    #[test]
    #[ignore = "requires signed Edge; native Navigator getters and real touch input"]
    fn private_native_navigator_acceptance() {
        let mut modes = Vec::new();
        for (cpu, touch, platform) in [
            (3, 0, PlatformKind::Windows),
            (7, 5, PlatformKind::Windows),
            (5, 0, PlatformKind::Linux),
        ] {
            let fixture = browser_fixture::ControlFixture::start(false);
            let browser = acceptance_browser_hardware(
                &fixture,
                "native-hardware",
                true,
                "native-controls",
                cpu,
                touch,
                platform,
            );
            let proofs = fixture.wait_proofs("/native-hardware/");
            assert_eq!(
                proofs
                    .iter()
                    .map(|(_, _, sample)| sample["kind"].as_str().unwrap())
                    .collect::<BTreeSet<_>>()
                    .len(),
                6
            );
            for (_, _, sample) in &proofs {
                browser.assert_signals(sample);
            }
            if touch > 0 {
                let page = browser
                    .controller
                    .targets
                    .0
                    .lock()
                    .unwrap()
                    .snapshot()
                    .into_iter()
                    .find(|target| target.kind == "page")
                    .unwrap();
                browser.controller.wire.call("Input.dispatchTouchEvent",json!({"type":"touchStart","touchPoints":[{"x":8,"y":8,"id":0},{"x":16,"y":16,"id":1}]}),Some(&page.session)).unwrap();
                browser
                    .controller
                    .wire
                    .call(
                        "Input.dispatchTouchEvent",
                        json!({"type":"touchEnd","touchPoints":[]}),
                        Some(&page.session),
                    )
                    .unwrap();
                let input = fixture.wait_kind_count("/native-hardware/", "touch_input", 1);
                assert_eq!(input.len(), 1);
                assert_eq!(input[0].2["trusted"], true);
                assert_eq!(input[0].2["nativeEvent"], true);
                assert_eq!(input[0].2["touches"], 2);
                assert_eq!(
                    evaluate(
                        &browser.controller.wire,
                        &page.session,
                        "({cpu:navigator.hardwareConcurrency,touch:navigator.maxTouchPoints})"
                    )
                    .unwrap(),
                    json!({"cpu":cpu,"touch":touch})
                );
                browser
                    .controller
                    .wire
                    .call(
                        "Page.reload",
                        json!({"ignoreCache":true}),
                        Some(&page.session),
                    )
                    .unwrap();
                let reloaded = fixture.wait_kind_count("/native-hardware/", "page", 2);
                assert_eq!(reloaded.len(), 2);
                for (_, _, sample) in reloaded {
                    browser.assert_signals(&sample);
                }
            }
            modes.push(json!({"hardwareConcurrency":cpu,"touchPoints":touch,
                "observations":proofs.iter().map(|(_, _, sample)|sample).collect::<Vec<_>>()}));
            if cpu == 5 {
                browser.write_report("native-navigator", json!(modes));
            }
        }
    }

    #[test]
    #[ignore = "requires signed Edge; native screen getters/CSS without resizing the viewport"]
    fn private_native_screen_acceptance() {
        let mut modes = Vec::new();
        for (width, height, scale, depth) in [(1440, 900, 125, 24), (900, 1440, 200, 16)] {
            let fixture = browser_fixture::ControlFixture::start(false);
            let browser = acceptance_browser_configured(
                &fixture,
                "native-screen",
                true,
                "native-screen-controls",
                |persona| {
                    persona.screen = ScreenConfig {
                        width,
                        height,
                        color_depth: depth,
                        device_scale_factor_hundredths: scale,
                    };
                },
            );
            let proofs = fixture.wait_proofs("/native-screen/");
            assert_eq!(
                proofs
                    .iter()
                    .map(|(_, _, sample)| sample["kind"].as_str().unwrap())
                    .collect::<BTreeSet<_>>()
                    .len(),
                6
            );
            for (_, _, sample) in &proofs {
                browser.assert_signals(sample);
                if !sample["screen"].is_null() {
                    assert_eq!(
                        sample["screen"]["orientation"],
                        if width < height {
                            "portrait-primary"
                        } else {
                            "landscape-primary"
                        }
                    );
                    assert!(
                        (sample["screen"]["dpr"].as_f64().unwrap() - f64::from(scale) / 100.0)
                            .abs()
                            < 0.001
                    );
                }
            }
            let page = browser
                .controller
                .targets
                .0
                .lock()
                .unwrap()
                .snapshot()
                .into_iter()
                .find(|target| target.kind == "page")
                .unwrap();
            let inspect = "({inner:[innerWidth,innerHeight],outer:[outerWidth,outerHeight],dpr:devicePixelRatio,sameGetters:['width','height','availWidth','availHeight'].every(name=>Object.getOwnPropertyDescriptor(Screen.prototype,name).get===globalThis.__ownedScreenGetters[name])})";
            let before = evaluate(&browser.controller.wire, &page.session, inspect).unwrap();
            assert_eq!(before["sameGetters"], true);
            browser
                .controller
                .wire
                .call(
                    "Emulation.clearDeviceMetricsOverride",
                    json!({}),
                    Some(&page.session),
                )
                .unwrap();
            let host_viewport = evaluate(&browser.controller.wire, &page.session, inspect).unwrap();
            assert_eq!(
                before, host_viewport,
                "screen emulation must not resize the real viewport/window or replace DPR/getters"
            );
            configure_native_screen(&browser.controller.wire, &page.session, &browser.screen)
                .unwrap();
            let restored = evaluate(&browser.controller.wire, &page.session, inspect).unwrap();
            assert_eq!(before, restored);
            browser
                .controller
                .wire
                .call(
                    "Page.reload",
                    json!({"ignoreCache":true}),
                    Some(&page.session),
                )
                .unwrap();
            let reloaded = fixture.wait_kind_count("/native-screen/", "page", 2);
            assert_eq!(reloaded.len(), 2);
            for (_, _, sample) in &reloaded {
                browser.assert_signals(sample);
            }
            modes.push(json!({"screenWidth":width,"screenHeight":height,"scaleHundredths":scale,
                "colorDepth":depth,"viewportPreserved":true,"getterIdentityPreserved":true,
                "observations":proofs.iter().map(|(_,_,sample)|sample).collect::<Vec<_>>(),
                "reloadObservations":reloaded.iter().map(|(_,_,sample)|sample).collect::<Vec<_>>()}));
            if width == 900 {
                browser.write_report("native-screen", json!(modes));
            }
        }
    }

    fn decode_gpu_png(encoded: &str) -> String {
        use base64::Engine;
        assert!(encoded.len() <= 16384);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().unwrap();
        assert_eq!((reader.info().width, reader.info().height), (16, 16));
        let size = reader.output_buffer_size().unwrap();
        assert!(size <= 1024);
        let mut output = vec![0; size];
        let info = reader.next_frame(&mut output).unwrap();
        let rgba = match info.color_type {
            png::ColorType::Rgba => output[..info.buffer_size()].to_vec(),
            png::ColorType::Rgb => output[..info.buffer_size()]
                .chunks_exact(3)
                .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
                .collect(),
            _ => panic!("expected fixed RGBA8/RGB8 fixture PNG"),
        };
        rgba.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    #[ignore = "closes only the owned loopback document connection; requires signed Edge"]
    fn private_navigation_error_result_acceptance() {
        let fixture = browser_fixture::ControlFixture::start(false);
        let browser = acceptance_browser(&fixture, "closed");
        assert!(!fixture
            .wait_kind_count("/closed/", "closed-response-fixture", 1)
            .is_empty());
        assert!(browser.controller.try_wait().unwrap().is_none());
        assert!(browser
            .controller
            .wire
            .call("Browser.getVersion", json!({}), None)
            .is_ok());
        assert!(browser
            .controller
            .wire
            .policy_failure
            .lock()
            .unwrap()
            .is_none());
        // The fixture returned no HTTP response, and supplied no page proof.
        assert!(!fixture
            .proofs
            .lock()
            .unwrap()
            .iter()
            .any(|(_, _, value)| value["kind"] == "page"));
    }

    #[test]
    #[ignore = "requires signed Edge and verifies real WebGL readback/export in isolated profiles"]
    fn private_webgl_pixel_acceptance() {
        let fixture = browser_fixture::ControlFixture::start(true);
        let mut modes = Vec::new();
        let mut first_samples: Option<Vec<Value>> = None;
        let mut native_availability = std::collections::BTreeMap::new();
        let mut native_rgba4 = std::collections::BTreeMap::new();
        for (route, noise, seed) in [
            ("gpu-native", false, "pixel-seed"),
            ("gpu-seeded", true, "pixel-seed"),
            ("gpu-reopen", true, "pixel-seed"),
            ("gpu-other", true, "different-pixel-seed"),
        ] {
            let browser = acceptance_browser_options(&fixture, route, noise, seed);
            let proofs = fixture.wait_proofs(&format!("/{route}/"));
            assert_eq!(
                proofs
                    .iter()
                    .map(|(_, _, v)| v["kind"].as_str().unwrap())
                    .collect::<BTreeSet<_>>()
                    .len(),
                9
            );
            let samples: Vec<Value> = proofs.into_iter().map(|(_, _, v)| v).collect();
            modes.push(json!({"mode":route,"observations":&samples}));
            // Preserve owned raw observations on assertion failure without
            // turning an incomplete matrix into a successful report.
            browser.write_report_status("webgl-pixels", json!(&modes), false);
            let page = samples.iter().find(|v| v["kind"] == "page").unwrap();
            for kind in ["webgl", "webgl2"] {
                assert!(
                    page["gpuPixels"][kind].is_object(),
                    "positive page GPU control required: {kind}"
                );
            }
            for sample in &samples {
                browser.assert_signals(sample);
                for kind in ["webgl", "webgl2"] {
                    let available = !sample["gpuPixels"][kind].is_null();
                    let key = (sample["kind"].as_str().unwrap().to_owned(), kind.to_owned());
                    if !noise {
                        native_availability.insert(key, available);
                    } else {
                        assert_eq!(
                            Some(&available),
                            native_availability.get(&key),
                            "GPU availability must be preserved"
                        );
                    }
                    if available {
                        if noise {
                            assert_ne!(
                                sample["gpuPixels"][kind]["pixels"],
                                json!("a42367ff".repeat(256))
                            );
                        } else {
                            assert_eq!(
                                sample["gpuPixels"][kind]["pixels"],
                                json!("a42367ff".repeat(256))
                            );
                        }
                        if kind == "webgl2" {
                            let renderbuffer = &sample["gpuPixels"][kind]["renderbuffer"];
                            let key = sample["kind"].as_str().unwrap().to_owned();
                            if noise {
                                assert_ne!(renderbuffer["pixels"], json!("a42367ff".repeat(63)));
                                assert_eq!(
                                    Some(&renderbuffer["nativeRgba4Pixels"]),
                                    native_rgba4.get(&key),
                                    "unsupported RGBA4 must preserve native pixels"
                                );
                            } else {
                                assert_eq!(renderbuffer["pixels"], json!("a42367ff".repeat(63)));
                                native_rgba4.insert(key, renderbuffer["nativeRgba4Pixels"].clone());
                            }
                        }
                    }
                }
            }
            if route == "gpu-reopen" || route == "gpu-other" {
                for sample in &samples {
                    let previous = first_samples
                        .as_ref()
                        .unwrap()
                        .iter()
                        .find(|v| v["kind"] == sample["kind"])
                        .unwrap();
                    for kind in ["webgl", "webgl2"] {
                        if sample["gpuPixels"][kind].is_null() {
                            continue;
                        }
                        if route == "gpu-reopen" {
                            assert_eq!(
                                sample["gpuPixels"][kind]["pixels"],
                                previous["gpuPixels"][kind]["pixels"]
                            );
                            if kind == "webgl2" {
                                assert_eq!(
                                    sample["gpuPixels"][kind]["renderbuffer"]["pixels"],
                                    previous["gpuPixels"][kind]["renderbuffer"]["pixels"]
                                );
                            }
                        } else {
                            assert_ne!(
                                sample["gpuPixels"][kind]["pixels"],
                                previous["gpuPixels"][kind]["pixels"]
                            );
                            if kind == "webgl2" {
                                assert_ne!(
                                    sample["gpuPixels"][kind]["renderbuffer"]["pixels"],
                                    previous["gpuPixels"][kind]["renderbuffer"]["pixels"]
                                );
                            }
                        }
                    }
                }
            }
            if route == "gpu-seeded" {
                first_samples = Some(samples.clone());
            }
            if route == "gpu-other" {
                browser.write_report("webgl-pixels", json!(modes));
            }
        }
    }

    #[test]
    #[ignore = "holds only a local test response until the controlled session has started"]
    fn private_pending_navigation_acceptance() {
        let fixture = browser_fixture::ControlFixture::start(false);
        let browser = acceptance_browser(&fixture, "held");
        assert!(
            fixture.wait_held_document(),
            "startup must request the document"
        );
        assert!(
            fixture.proofs.lock().unwrap().is_empty(),
            "the document response is still held"
        );
        assert!(browser.controller.try_wait().unwrap().is_none());
        let page = browser
            .controller
            .targets
            .0
            .lock()
            .unwrap()
            .snapshot()
            .into_iter()
            .find(|target| target.kind == "page")
            .unwrap();
        // This command is handled by the browser. Renderer-side evaluation may
        // legitimately be suspended until the held navigation commits.
        let history = browser
            .controller
            .wire
            .call("Page.getNavigationHistory", json!({}), Some(&page.session))
            .unwrap();
        assert_eq!(
            history["entries"].as_array().unwrap().len(),
            1,
            "startup initialization must not introduce an extra history entry"
        );
        assert_eq!(history["currentIndex"], 0);
        assert_eq!(history["entries"][0]["url"], "about:blank");
        fixture.release_document();
        let proofs = fixture.wait_proofs("/held/");
        assert_eq!(
            proofs
                .iter()
                .map(|(_, _, v)| v["kind"].as_str().unwrap())
                .collect::<BTreeSet<_>>()
                .len(),
            6
        );
        for (_, _, first) in &proofs {
            browser.assert_signals(first);
        }
        browser.write_report("pending-navigation", json!({"startedBeforeDocumentResponse":true,"observations":proofs.iter().map(|(_, _, value)| value).collect::<Vec<_>>()}));
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
    #[ignore = "requires signed Edge; fail-fast fresh-browser startup stress without retries"]
    fn private_startup_navigation_stress_acceptance() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/fingerprint-startup-navigation-stress-acceptance.json");
        for completed in 0..=30 {
            let report = json!({"assertionsPassed":completed==30,"complete":false,
                "completedRounds":completed,"requiredRounds":30,
                "scope":"fresh signed Edge and owned nine-context module fixture"});
            std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
            if completed == 30 {
                break;
            }
            eprintln!("Fresh browser startup {} / 30", completed + 1);
            private_module_worker_acceptance();
        }
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
