import assert from 'node:assert/strict';
import test from 'node:test';
import {readFile} from 'node:fs/promises';
import {stripTypeScriptTypes} from 'node:module';
import vm from 'node:vm';
const context = vm.createContext({Error,TextEncoder});
const module = new vm.SourceTextModule(stripTypeScriptTypes(await readFile(new URL('../src/profile-batch.ts',import.meta.url),'utf8'),{mode:'transform'}),{context});
await module.link(()=>{throw Error('No runtime dependencies expected');}); await module.evaluate();
const {runProfileBatch,parseBatchTags,MAX_BATCH_PROFILES}=module.namespace;
const plain=value=>JSON.parse(JSON.stringify(value));
const profile=(id,status='stopped')=>({id,name:id,status,revision:4,tags:['QA'],proxyId:'proxy-original',persona:{kind:'seedable_noise',seedable:{seed:'unique-'+id}}});
const unexpected=()=>{throw Error('Unexpected operation');};
function api(changes={}){return {launchSession:unexpected,stopSession:unexpected,updateProfile:unexpected,...changes};}
test('launch preserves selection order, records queueing and continues after individual failure', async()=>{
  const calls=[],rows=[profile('a'),profile('bad'),profile('q'),profile('active','running')];
  const results=await runProfileBatch(api({launchSession:async id=>{calls.push(id);if(id==='bad')throw Error('launch failed');return profile(id,id==='q'?'queued':'running');}}),rows,{kind:'launch'});
  assert.deepEqual(calls,['a','bad','q']); assert.deepEqual(plain(results.map(r=>r.status)),['success','failed','success','skipped']);
  assert.equal(results[2].message,'已排队'); assert.equal(results[1].message,'launch failed');
});
test('selected queued tickets are cancelled before active sessions can promote them',async()=>{
  const calls=[],queued=new Set(['q1','q2']);let promoted=0;
  const results=await runProfileBatch(api({stopSession:async id=>{calls.push(id);if(queued.has(id))queued.delete(id);else promoted+=queued.size;return profile(id);}}),[profile('active','running'),profile('q1','queued'),profile('q2','queued')],{kind:'stop'});
  assert.deepEqual(calls,['q1','q2','active']);assert.equal(promoted,0);assert.equal(results.every(r=>r.status==='success'),true);
});
test('batch edits preserve persona and use each optimistic revision without retrying conflicts',async()=>{
  const rows=[profile('a'),profile('conflict'),profile('active','running')],before=JSON.stringify(rows),calls=[];
  const results=await runProfileBatch(api({updateProfile:async(id,revision,changes)=>{calls.push({id,revision,changes:plain(changes)});if(id==='conflict')throw Error('revision conflict');return profile(id);}}),rows,{kind:'edit',addTags:['qa','new'],proxyId:null});
  assert.equal(JSON.stringify(rows),before);assert.equal(calls.length,2);
  assert.deepEqual(calls[0],{id:'a',revision:4,changes:{tags:['QA','new'],proxyId:''}});
  assert.deepEqual(plain(results.map(r=>r.status)),['success','failed','skipped']);
});
test('cancellation prevents every remaining mutation and duplicate IDs execute once',async()=>{
  let cancelled=false;const calls=[];
  const results=await runProfileBatch(api({launchSession:async id=>{calls.push(id);cancelled=true;return profile(id,'running');}}),[profile('a'),profile('a'),profile('b')],{kind:'launch'},{cancelled:()=>cancelled});
  assert.deepEqual(calls,['a']);assert.deepEqual(plain(results.map(r=>r.status)),['success','cancelled']);
});
test('no-op edits skip writes; validation rejects oversized batches and UTF-8 tags',async()=>{
  const results=await runProfileBatch(api(),[profile('a')],{kind:'edit',addTags:['qa']});assert.equal(results[0].status,'skipped');
  assert.deepEqual(plain(parseBatchTags(' QA,qa，new\nnew ')),['qa','new']);
  assert.throws(()=>parseBatchTags('甲'.repeat(22)));
  await assert.rejects(runProfileBatch(api(),Array.from({length:MAX_BATCH_PROFILES+1},(_,i)=>profile(String(i))),{kind:'launch'}));
  await assert.rejects(runProfileBatch(api(),[profile('a')],{kind:'edit',addTags:[]}));
});
