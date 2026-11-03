//! Session-scoped control and observation over private inherited pipes.
//! No generic command/evaluate surface is exposed through application ports.
use super::{internal, invalid_data, now_ms, win_pipe};
use brows_application::{PortResult, SessionObservation, TargetObservation};
use brows_domain::{PlatformKind, SessionFingerprintConfig, SessionId};
use serde_json::{json, Value};
use std::{collections::BTreeMap, ffi::OsString, io::{Read, Write}, path::Path, sync::{atomic::{AtomicBool, AtomicU64, Ordering}, mpsc, Arc, Condvar, Mutex}, thread::{self, JoinHandle}, time::{Duration, Instant}};
const TIMEOUT:Duration=Duration::from_secs(8);
const MAX_FRAME:usize=2*1024*1024;
const MAX_TARGETS:usize=128;
#[derive(Debug)]
struct Wire {
 process:Arc<win_pipe::Process>, next:AtomicU64, closing:AtomicBool,
 writer:mpsc::SyncSender<Vec<u8>>,
 pending:Mutex<BTreeMap<u64,mpsc::Sender<Value>>>,
}
impl Wire {
 fn fail(&self) { self.closing.store(true,Ordering::Release); let _=self.process.kill(); if let Ok(mut p)=self.pending.lock() { p.clear(); } }
 fn call(&self,method:&str,params:Value,session:Option<&str>)->PortResult<Value> {
  if self.closing.load(Ordering::Acquire) { return Err(internal("session control is closed")); }
  let id=self.next.fetch_add(1,Ordering::Relaxed); let mut frame=json!({"id":id,"method":method,"params":params});
  if let Some(s)=session { frame["sessionId"]=json!(s); }
  let mut bytes=serde_json::to_vec(&frame).map_err(|_| internal("cannot encode session policy"))?; bytes.push(0);
  if bytes.len()>64*1024 { return Err(invalid_data("session control message exceeds limit")); }
  let (send,receive)=mpsc::channel();
  { let mut pending=self.pending.lock().map_err(|_| internal("session replies lock poisoned"))?;
    if pending.len()>=64 { return Err(internal("too many pending session commands")); } pending.insert(id,send); }
  if self.writer.try_send(bytes).is_err() { self.fail(); return Err(internal("session control writer unavailable")); }
  let result=receive.recv_timeout(TIMEOUT);
  if let Ok(mut p)=self.pending.lock() { p.remove(&id); }
  let value=result.map_err(|_| { self.fail(); internal("session control response timeout") })?;
  if value.get("error").is_some() { return Err(internal(format!("browser cannot apply {method}"))); }
  Ok(value["result"].clone())
 }
}
#[derive(Debug,Clone)] struct Target { session:String, kind:String }
#[derive(Debug,Default)] struct Targets { configured:BTreeMap<String,Target> }
#[derive(Debug)]
pub(super) struct Controller {
 wire:Arc<Wire>, targets:Arc<(Mutex<Targets>,Condvar)>, threads:Vec<JoinHandle<()>>,
}
impl Controller {
 pub(super) fn start(executable:&Path,args:&[OsString],policy:SessionFingerprintConfig,urls:&[String])->PortResult<Self> {
  let (process,mut output,mut input)=win_pipe::spawn(executable,args).map_err(|_| internal("cannot start private session control"))?;
  let (writer,write_receive)=mpsc::sync_channel::<Vec<u8>>(8);
  let (events,event_receive)=mpsc::sync_channel::<Value>(128);
  let wire=Arc::new(Wire { process:Arc::new(process), next:AtomicU64::new(1), closing:AtomicBool::new(false), writer, pending:Mutex::new(BTreeMap::new()) });
  let targets=Arc::new((Mutex::new(Targets::default()),Condvar::new()));
  let w=wire.clone(); let write_thread=thread::spawn(move || {
   while !w.closing.load(Ordering::Acquire) { match write_receive.recv_timeout(Duration::from_millis(200)) {
    Ok(frame)=>if output.write_all(&frame).is_err() { w.fail(); break; },
    Err(mpsc::RecvTimeoutError::Timeout)=>{}, Err(_)=>break,
   }}
  });
  let w=wire.clone(); let read_thread=thread::spawn(move || {
   let mut frame=Vec::new(); let mut chunk=[0u8;4096];
   'read: loop { let size=match input.read(&mut chunk) { Ok(0)|Err(_)=>break, Ok(size)=>size };
    for byte in &chunk[..size] { if *byte==0 {
      let value:Value=match serde_json::from_slice(&frame) { Ok(v)=>v,Err(_)=>break 'read }; frame.clear();
      if let Some(id)=value["id"].as_u64() {
       if let Ok(mut pending)=w.pending.lock() { if let Some(sender)=pending.remove(&id) { let _=sender.send(value); } }
      } else if matches!(value["method"].as_str(),Some("Target.attachedToTarget"|"Target.detachedFromTarget")) && events.try_send(value).is_err() { break 'read; }
     } else { frame.push(*byte); if frame.len()>MAX_FRAME { break 'read; } }
    }
   }
   w.fail();
  });
  let w=wire.clone(); let t=targets.clone(); let actor=thread::spawn(move || {
   while !w.closing.load(Ordering::Acquire) {
    let event=match event_receive.recv_timeout(Duration::from_millis(200)) { Ok(v)=>v,Err(mpsc::RecvTimeoutError::Timeout)=>continue,Err(_)=>break };
    if event["method"]=="Target.detachedFromTarget" {
     if let Some(session)=event["params"]["sessionId"].as_str() { if let Ok(mut state)=t.0.lock() { state.configured.retain(|_,value| value.session!=session); } }
     continue;
    }
    let params=&event["params"]; let Some(session)=params["sessionId"].as_str() else { w.fail();break; };
    let Some(id)=params["targetInfo"]["targetId"].as_str() else { w.fail();break; };
    let kind=params["targetInfo"]["type"].as_str().unwrap_or("");
    let url=params["targetInfo"]["url"].as_str().unwrap_or("");
    if url.starts_with("chrome-extension://") || url.starts_with("edge://") {
        if w.call("Runtime.runIfWaitingForDebugger",json!({}),Some(session)).is_err() { w.fail();break; }
        continue;
    }
    if configure(&w,session,kind,&policy).is_err() { w.fail(); t.1.notify_all(); break; }
    { let Ok(mut state)=t.0.lock() else { w.fail();break; };
      if state.configured.len()>=MAX_TARGETS { w.fail();break; }
      state.configured.insert(id.to_owned(),Target{session:session.to_owned(),kind:kind.to_owned()}); }
    t.1.notify_all();
    if w.call("Runtime.runIfWaitingForDebugger",json!({}),Some(session)).is_err() { w.fail();break; }
   }
  });
  let controller=Self{wire,targets,threads:vec![write_thread,read_thread,actor]};
  controller.wire.call("Browser.getVersion",json!({}),None)?;
  controller.wire.call("Target.setAutoAttach",auto_attach(),None)?;
  for url in if urls.is_empty() { vec!["about:blank".to_owned()] } else { urls.to_vec() } {
   let target=controller.wire.call("Target.createTarget",json!({"url":"about:blank"}),None)?;
   let id=target["targetId"].as_str().ok_or_else(|| internal("browser omitted target identity"))?;
   let target=controller.await_target(id)?;
   if url!="about:blank" { controller.wire.call("Page.navigate",json!({"url":url}),Some(&target.session))?; }
  }
  Ok(controller)
 }
 fn await_target(&self,id:&str)->PortResult<Target> {
  let deadline=Instant::now()+TIMEOUT; let mut targets=self.targets.0.lock().map_err(|_| internal("target lock poisoned"))?;
  loop {
   if let Some(target)=targets.configured.get(id) { return Ok(target.clone()); }
   if self.wire.closing.load(Ordering::Acquire) || Instant::now()>=deadline { return Err(internal("browser target policy failed before navigation")); }
   targets=self.targets.1.wait_timeout(targets,Duration::from_millis(100)).map_err(|_| internal("target lock poisoned"))?.0;
  }
 }
 pub(super) fn id(&self)->u32 { self.wire.process.id() }
 pub(super) fn kill(&self)->std::io::Result<()> { self.wire.fail(); Ok(()) }
 pub(super) fn wait(&self)->std::io::Result<()> { self.wire.process.wait() }
 pub(super) fn try_wait(&self)->std::io::Result<Option<u32>> { self.wire.process.try_wait() }
 pub(super) fn observe(&self,session_id:&SessionId)->PortResult<SessionObservation> {
  let targets:Vec<_>=self.targets.0.lock().map_err(|_| internal("target lock poisoned"))?.configured.values().cloned().collect();
  if targets.len()>32 { return Err(internal("session observation exceeds target limit")); }
  let mut result=Vec::new();
  for target in targets {
   let signals=evaluate(&self.wire,&target.session,OBSERVE)?;
   result.push(TargetObservation {
    kind:target.kind, user_agent:string(&signals,"userAgent")?, platform:string(&signals,"platform")?,
    languages:signals["languages"].as_array().ok_or_else(|| internal("invalid language observation"))?.iter().map(|v| v.as_str().map(str::to_owned).ok_or_else(|| internal("invalid language observation"))).collect::<PortResult<_>>()?,
    hardware_concurrency:signals["hardwareConcurrency"].as_u64().ok_or_else(|| internal("invalid hardware observation"))?,
    device_memory:signals["deviceMemory"].to_string(), timezone:string(&signals,"timezone")?,client_hints_platform:signals["clientHintsPlatform"].as_str().unwrap_or("").to_owned(),
   });
  }
  Ok(SessionObservation{session_id:session_id.clone(),observed_at_ms:now_ms(),targets:result})
 }
}
impl Drop for Controller { fn drop(&mut self) { self.wire.fail(); for thread in self.threads.drain(..) { let _=thread.join(); } } }
fn string(value:&Value,key:&str)->PortResult<String> { value[key].as_str().filter(|s| s.len()<=2048).map(str::to_owned).ok_or_else(|| internal("invalid session observation")) }
fn evaluate(wire:&Wire,session:&str,expression:&str)->PortResult<Value> {
 let result=wire.call("Runtime.evaluate",json!({"expression":expression,"returnByValue":true,"silent":true}),Some(session))?;
 if result.get("exceptionDetails").is_some() { return Err(internal("session observation could not read the requested APIs")); }
 Ok(result["result"]["value"].clone())
}
fn auto_attach()->Value { json!({"autoAttach":true,"waitForDebuggerOnStart":true,"flatten":true,"filter":[{"type":"page"},{"type":"iframe"},{"type":"worker"},{"type":"shared_worker"},{"type":"service_worker"}]}) }
fn configure(wire:&Wire,session:&str,kind:&str,policy:&SessionFingerprintConfig)->PortResult<()> {
 wire.call("Target.setAutoAttach",auto_attach(),Some(session))?;
 let (platform,platform_version)=match policy.platform { PlatformKind::Windows=>("Windows","10.0.0"),PlatformKind::MacOs=>("macOS","10.15.7"),PlatformKind::Linux=>("Linux","") };
 let platform_string=match policy.platform { PlatformKind::Windows=>"Win32",PlatformKind::MacOs=>"MacIntel",PlatformKind::Linux=>"Linux x86_64" };
 let version=policy.user_agent.split("Chrome/").nth(1).and_then(|s| s.split(' ').next()).ok_or_else(|| invalid_data("missing Chromium version in policy"))?;
 let major=version.split('.').next().unwrap_or_default();
 wire.call("Network.setUserAgentOverride",json!({"userAgent":policy.user_agent,"acceptLanguage":policy.languages.join(","),"platform":platform_string,
  "userAgentMetadata":{"brands":[{"brand":"Chromium","version":major},{"brand":"Google Chrome","version":major}],"fullVersionList":[{"brand":"Chromium","version":version},{"brand":"Google Chrome","version":version}],"fullVersion":version,"platform":platform,"platformVersion":platform_version,"architecture":"x86","bitness":"64","model":"","mobile":false,"wow64":false}}),Some(session))?;
 let is_page=kind=="page"||kind=="iframe";
 if let Some(timezone)=&policy.timezone {
  let result=wire.call("Emulation.setTimezoneOverride",json!({"timezoneId":timezone}),Some(session));
  if result.is_err() && (is_page || evaluate(wire,session,"Intl.DateTimeFormat().resolvedOptions().timeZone")?.as_str()!=Some(timezone)) {
   return Err(internal("native timezone is unavailable for this target"));
  }
 }
 {
  let memory=policy.device_memory.reported_f64();
  let values=json!({"hardwareConcurrency":policy.hardware_concurrency,"deviceMemory":memory,"languages":policy.languages,"language":policy.languages[0],"platform":platform_string});
  let script=format!("(()=>{{const values={values};const p=Object.getPrototypeOf(navigator);for(const [key,value] of Object.entries(values)){{const d=Object.getOwnPropertyDescriptor(p,key);if(!d?.get||!d.configurable)throw new Error('Unsupported worker property');const original=d.get;Object.defineProperty(p,key,{{...d,get(){{original.call(this);return Array.isArray(value)?Object.freeze([...value]):value;}}}});}}return navigator.hardwareConcurrency===values.hardwareConcurrency&&navigator.deviceMemory===values.deviceMemory;}})()");
  if is_page { wire.call("Page.addScriptToEvaluateOnNewDocument",json!({"source":script}),Some(session))?; }
  if evaluate(wire,session,&script)?!=true { return Err(internal("target hardware policy did not apply")); }
 }
 Ok(())
}
const OBSERVE:&str="(()=>({userAgent:navigator.userAgent,platform:navigator.platform,languages:Array.from(navigator.languages),hardwareConcurrency:navigator.hardwareConcurrency,deviceMemory:navigator.deviceMemory,timezone:Intl.DateTimeFormat().resolvedOptions().timeZone,clientHintsPlatform:navigator.userAgentData?.platform??''}))()";
