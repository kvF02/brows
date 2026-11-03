import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import vm from 'node:vm';
import test from 'node:test';
const config={origin:'https://probe.example',zone:'probe.example',stunMin:40000,stunMax:40127};
const source=(await readFile(new URL('../crates/adapters-runtime/src/network_probe.js',import.meta.url),'utf8')).replace('__BROWS_SERVICE__',JSON.stringify(config));
async function execute(fetch){
 const context=vm.createContext({fetch,URL,AbortSignal,TextDecoder,Uint8Array,setTimeout,clearTimeout});
 vm.runInContext(source,context);
 for(let n=0;n<100&&!context.__browsNetworkProbe;n++)await new Promise(resolve=>setImmediate(resolve));
 assert.ok(context.__browsNetworkProbe,'probe must terminate with explicit evidence state');
 return JSON.parse(JSON.stringify(context.__browsNetworkProbe));
}
test('a malformed challenge cannot redirect the browser probe to another host or port',async()=>{
 for(const change of [{hostname:'private.example'},{stunPort:22}]){
  let calls=0;
  const result=await execute(async(url,options)=>{
   calls++;assert.equal(url,config.origin+'/v1/challenge');assert.equal(options.credentials,'omit');assert.equal(options.redirect,'error');
   return new Response(JSON.stringify({token:'a'.repeat(32),hostname:'a'.repeat(32)+'.probe.example',stunPort:40001,...change}));
  });
  assert.equal(calls,1);assert.deepEqual(result,{dns:'not_checked',webRtc:'not_checked',httpEgress:'not_checked'});
 }
});
test('missing ICE support still reports independent results; invalid reports never pass',async()=>{
 for(const invalid of [false,true]){
  const calls=[];const token='b'.repeat(32);
  const result=await execute(async(url,options)=>{
   calls.push(url);assert.equal(options.credentials,'omit');
   if(url.endsWith('/v1/challenge'))return new Response(JSON.stringify({token,hostname:token+'.probe.example',stunPort:40001}));
   if(url.endsWith('/v1/ping'))return new Response('ok');
   assert.deepEqual(JSON.parse(options.body),{srflx:[]});
   return new Response(JSON.stringify({dns:invalid?'all-good':'fail',webRtc:'not_checked',httpEgress:'pass'}));
  });
  assert.equal(calls.length,3);
  assert.deepEqual(result,invalid?{dns:'not_checked',webRtc:'not_checked',httpEgress:'not_checked'}:{dns:'fail',webRtc:'not_checked',httpEgress:'pass'});
 }
});
