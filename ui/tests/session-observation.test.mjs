import assert from 'node:assert/strict';
import test from 'node:test';
import { loadAdapter } from './load-adapter.mjs';
const {TauriApiClient}=await loadAdapter('tauri');
const {MockApiClient}=await loadAdapter('mock');
test('session observation resolves the running session and exposes no generic evaluate command',async()=>{
 const calls=[]; const report={sessionId:'running',observedAtMs:123,targets:[]};
 const api=new TauriApiClient({invoke:async(handler,args)=>{calls.push({handler,args});
  if(handler==='session_list')return [{id:'running',profileId:'p',state:'running',createdAtMs:1},{id:'old',profileId:'p',state:'failed',createdAtMs:0}];
  if(handler==='session_observe'){assert.deepEqual(JSON.parse(JSON.stringify(args)),{request:{sessionId:'running'}});return report;}
  throw Error(handler);
 }});
 assert.equal(await api.observeSession('p'),report);
 assert.deepEqual(calls.map(c=>c.handler),['session_list','session_observe']);
});
test('queued and stopped sessions cannot be observed',async()=>{
 for(const state of ['queued','stopped','failed']){
  const api=new TauriApiClient({invoke:async(handler)=>{assert.equal(handler,'session_list');return [{id:'s',profileId:'p',state,createdAtMs:1}];}});
  await assert.rejects(api.observeSession('p'),e=>e.code==='INVALID_PROFILE_STATE');
 }
});
test('browser mock refuses to claim real session measurements',async()=>{
 await assert.rejects(new MockApiClient().observeSession('profile-demo-1'),e=>e.code==='CAPABILITY_UNSUPPORTED');
});
test('network diagnostics use operator configuration and a minimal acknowledged session request',async()=>{
 const service={origin:'https://probe.example',zone:'probe.example',stunMin:40000,stunMax:40127};
 const report={dns:'not_checked',webRtc:'fail',httpEgress:'pass'};
 const api=new TauriApiClient({invoke:async(handler,args)=>{
  if(handler==='network_probe_service')return service;
  if(handler==='session_list')return [{id:'s',profileId:'p',state:'running',createdAtMs:1}];
  assert.equal(handler,'session_probe_network');
  assert.deepEqual(JSON.parse(JSON.stringify(args)),{request:{sessionId:'s',acknowledgeExternal:true}});
  return report;
 }});
 assert.equal(await api.getNetworkProbeService(),service);
 assert.equal(await api.probeSessionNetwork('p'),report);
 const mock=new MockApiClient();assert.equal(await mock.getNetworkProbeService(),undefined);
 await assert.rejects(mock.probeSessionNetwork('profile-demo-1'),e=>e.code==='CAPABILITY_UNSUPPORTED');
});
