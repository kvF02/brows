import assert from 'node:assert/strict';
import test from 'node:test';
import {loadAdapter} from './load-adapter.mjs';
const {TauriApiClient}=await loadAdapter('tauri');
const persona={kind:'seedable_noise',seedable:{seed:'zone',platform:'windows',hardwareConcurrency:4,deviceMemory:'4',touchPoints:0,screenWidth:1920,screenHeight:1080,colorDepth:24,deviceScaleFactor:1,webglVendor:'',webglRenderer:'',spoofCanvas:true,spoofAudio:false}};
test('timezone round-trips through drafts and partial edits; SystemNative clears it',async()=>{
 let stored;const api=new TauriApiClient({invoke:async(handler,args)=>{
  if(handler==='profile_create'){stored={...args.request.profile,id:'p',revision:1,archived:false};return stored;}
  if(handler==='profile_update'){stored={...args.request.profile,id:'p',revision:stored.revision+1,archived:false};return stored;}
  throw Error(handler);
 }});
 const created=await api.createProfile({name:'zone',tags:[],presetId:'system-native',runtimeId:'edge',persona,timezone:'Asia/Tokyo',acceptLanguages:['ja-JP']});
 assert.equal(created.timezone,'Asia/Tokyo');
 const renamed=await api.updateProfile(created.id,created.revision,{name:'renamed'});
 assert.equal(renamed.timezone,'Asia/Tokyo');
 const native=await api.updateProfile(created.id,renamed.revision,{persona:{kind:'system_native'}});
 assert.equal(native.timezone,undefined);
 assert.equal(stored.locale.timezone,undefined);
});
