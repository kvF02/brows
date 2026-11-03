import assert from 'node:assert/strict';
import {createSocket} from 'node:dgram';
import test from 'node:test';
import {crc32,allowed,stunReply,dnsQuestion,dnsReply,assessNetwork,startProbeService} from './network-probe-service.mjs';
test('CIDR rules and no-observation cases do not manufacture passing evidence',()=>{
 assert.equal(allowed('203.0.113.7',['203.0.113.0/24']),true);assert.equal(allowed('203.0.114.7',['203.0.113.0/24']),false);
 assert.equal(allowed('::ffff:127.0.0.1',['127.0.0.1/32']),true);
 for(const cidr of ['127.0.0.1/', '127.0.0.1/24/ignored', '127.0.0.1/-1'])assert.throws(()=>allowed('127.0.0.1',[cidr]),/Invalid IPv4 CIDR/);
 const config={dnsCidrs:['192.0.2.0/24'],egressCidrs:['203.0.113.0/24']};
 const result=assessNetwork({dns:new Set(),stun:new Set(),http:new Set()},config);
 assert.equal(result.dns,'not_checked');assert.equal(result.webRtc,'not_checked');
 const leak=assessNetwork({dns:new Set(['198.51.100.1']),stun:new Set(['198.51.100.2']),http:new Set(['203.0.113.1'])},config);
 assert.equal(leak.dns,'fail');assert.equal(leak.webRtc,'fail');assert.equal(leak.httpEgress,'pass');
 const expected=assessNetwork({dns:new Set(['192.0.2.1']),stun:new Set(['203.0.113.1']),http:new Set()},config);
 assert.equal(expected.webRtc,'not_checked','STUN success also needs browser candidate correlation');
 assert.equal(assessNetwork({dns:new Set(['192.0.2.1']),dnsOverflow:true,stun:new Set(),http:new Set()},config).dns,'not_checked','bounded samples cannot conceal unobserved addresses');
});
test('STUN response preserves transaction and correctly encodes XOR-MAPPED-ADDRESS',()=>{
 const request=Buffer.alloc(20);request.writeUInt16BE(1);request.writeUInt32BE(0x2112a442,4);request.fill(7,8);
 const reply=stunReply(request,'203.0.113.8',43210);
 assert.equal(reply.readUInt16BE(0),0x0101);assert.equal(reply.subarray(8,20).toString('hex'),request.subarray(8).toString('hex'));
 assert.equal(reply.readUInt16BE(26)^(0x2112a442>>>16),43210);assert.equal((reply.readUInt32BE(28)^0x2112a442)>>>0,0xcb007108);
 assert.equal(stunReply(Buffer.alloc(10000),'203.0.113.8',9),null);
});
test('DNS parser rejects compression and truncation; answers only the active challenge',()=>{
 const labels=Buffer.concat(['abcd','probe','example'].map(v=>Buffer.concat([Buffer.from([v.length]),Buffer.from(v)])));
 const packet=Buffer.concat([Buffer.alloc(12),labels,Buffer.from([0,0,1,0,1])]);packet.writeUInt16BE(1,4);
 const q=dnsQuestion(packet);assert.equal(q.name,'abcd.probe.example');
 assert.equal(dnsReply(packet,q,'203.0.113.1',true).readUInt16BE(6),1);
 assert.equal(dnsReply(packet,q,'203.0.113.1',false).readUInt16BE(2)&15,3);
 packet[12]=0xc0;assert.equal(dnsQuestion(packet),null);
 assert.equal(dnsQuestion(Buffer.alloc(3)),null);
});
test('loopback service binds and expires isolated challenges without claiming an Internet test',async()=>{
 const service=await startProbeService({zone:'probe.example',publicIPv4:'127.0.0.1',dnsCidrs:['127.0.0.0/8'],egressCidrs:['127.0.0.0/8'],stunMin:0,stunMax:0,dnsPort:0,apiPort:0});
 const client=createSocket('udp4');
 try{
  const base=`http://127.0.0.1:${service.apiPort}`;const challenge=await(await fetch(base+'/v1/challenge',{method:'POST'})).json();
  assert.match(challenge.hostname,/^[0-9a-f]{32}\.probe\.example$/);
  const request=Buffer.alloc(20);request.writeUInt16BE(1);request.writeUInt32BE(0x2112a442,4);
  const response=new Promise((yes,no)=>{const timer=setTimeout(()=>no(Error('STUN timeout')),2000);client.once('message',value=>{clearTimeout(timer);yes(value);});});
  client.send(request,challenge.stunPort,'127.0.0.1');assert.equal((await response).readUInt16BE(0),0x0101);
  const result=await(await fetch(base+'/v1/result/'+challenge.token,{method:'POST',body:JSON.stringify({srflx:['127.0.0.1']})})).json();
  assert.equal(result.webRtc,'pass');assert.equal(result.dns,'not_checked');
  assert.equal((await fetch(base+'/v1/result/'+challenge.token,{method:'POST'})).status,404);
 }finally{client.close();await service.close();}
});

test('STUN optional attributes and fingerprint are accepted and verified',()=>{
 assert.equal(crc32(Buffer.from('123456789')),0xcbf43926);
 const request=Buffer.alloc(36);request.writeUInt16BE(1);request.writeUInt16BE(16,2);request.writeUInt32BE(0x2112a442,4);
 request.writeUInt16BE(0x8022,20);request.writeUInt16BE(4,22);request.write('test',24);request.writeUInt16BE(0x8028,28);request.writeUInt16BE(4,30);request.writeUInt32BE((crc32(request.subarray(0,28))^0x5354554e)>>>0,32);
 const reply=stunReply(request,'203.0.113.8',1234);assert.equal(reply.length,40);assert.equal(reply.readUInt32BE(36),(crc32(reply.subarray(0,32))^0x5354554e)>>>0);
 request[32]^=1;assert.equal(stunReply(request,'203.0.113.8',1234),null);
});
