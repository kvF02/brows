// Optional self-hosted IPv4 DNS/STUN correlation service. No analytics or logs.
import {createServer as httpServer} from 'node:http';
import {createServer as httpsServer} from 'node:https';
import {createSocket} from 'node:dgram';
import {randomBytes} from 'node:crypto';
import {isIP} from 'node:net';
import {readFile} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
import {resolve} from 'node:path';
const COOKIE=0x2112a442;
export function ipv4(value) {
 if(value.startsWith('::ffff:'))value=value.slice(7);
 if(isIP(value)!==4)return null;
 return value.split('.').reduce((n,part)=>((n<<8)>>>0)+Number(part),0)>>>0;
}
export function allowed(address,cidrs) {
 const ip=ipv4(address); if(ip===null)return false;
 return cidrs.some(cidr=>{const parts=cidr.split('/');const [base,bits='32']=parts;const value=ipv4(base);const n=Number(bits);
  if(parts.length>2||!/^\d{1,2}$/.test(bits)||value===null||!Number.isInteger(n)||n<0||n>32)throw Error('Invalid IPv4 CIDR');
  const mask=n===0?0:(0xffffffff<<(32-n))>>>0;return (ip&mask)===(value&mask);
 });
}
export function crc32(bytes) {
 let crc=0xffffffff;
 for(const byte of bytes){crc^=byte;for(let bit=0;bit<8;bit++)crc=(crc>>>1)^((crc&1)?0xedb88320:0);}
 return (crc^0xffffffff)>>>0;
}
export function stunReply(packet,address,port) {
 if(packet.length<20||packet.length>512||packet.readUInt16BE(0)!==1||packet.readUInt16BE(2)!==packet.length-20||packet.readUInt32BE(4)!==COOKIE||ipv4(address)===null)return null;
 let fingerprint=false;
 for(let offset=20;offset<packet.length;){if(offset+4>packet.length)return null;const type=packet.readUInt16BE(offset),size=packet.readUInt16BE(offset+2);const end=offset+4+Math.ceil(size/4)*4;if(end>packet.length)return null;
  if(type===0x8028){if(size!==4||end!==packet.length||packet.readUInt32BE(offset+4)!==((crc32(packet.subarray(0,offset))^0x5354554e)>>>0))return null;fingerprint=true;}offset=end;}
 const reply=Buffer.alloc(fingerprint?40:32);reply.writeUInt16BE(0x0101,0);reply.writeUInt16BE(fingerprint?20:12,2);reply.writeUInt32BE(COOKIE,4);packet.copy(reply,8,8,20);
 reply.writeUInt16BE(0x0020,20);reply.writeUInt16BE(8,22);reply[25]=1;reply.writeUInt16BE(port^(COOKIE>>>16),26);reply.writeUInt32BE((ipv4(address)^COOKIE)>>>0,28);
 if(fingerprint){reply.writeUInt16BE(0x8028,32);reply.writeUInt16BE(4,34);reply.writeUInt32BE((crc32(reply.subarray(0,32))^0x5354554e)>>>0,36);}
 return reply;
}
export function dnsQuestion(packet) {
 if(packet.length<17||packet.length>1232||packet.readUInt16BE(4)!==1||(packet.readUInt16BE(2)&0xf800)!==0)return null;
 const labels=[];let offset=12;
 while(offset<packet.length){const size=packet[offset++];if(size===0)break;if(size>63||offset+size>packet.length)return null;
  const label=packet.subarray(offset,offset+size).toString('ascii');if(!/^[a-zA-Z0-9-]+$/.test(label))return null;labels.push(label.toLowerCase());offset+=size;
 }
 if(offset+4>packet.length||labels.join('.').length>253)return null;
 return {name:labels.join('.'),type:packet.readUInt16BE(offset),qclass:packet.readUInt16BE(offset+2),end:offset+4};
}
export function dnsReply(packet,question,address,exists) {
 const answer=exists&&question.type===1&&question.qclass===1;
 const response=Buffer.alloc(question.end+(answer?16:0));packet.copy(response,0,0,question.end);
 response.writeUInt16BE(0x8400|(packet.readUInt16BE(2)&0x0100)|(exists?0:3),2);
 response.writeUInt16BE(answer?1:0,6);response.writeUInt16BE(0,8);response.writeUInt16BE(0,10);
 if(answer){let n=question.end;response.writeUInt16BE(0xc00c,n);response.writeUInt16BE(1,n+2);response.writeUInt16BE(1,n+4);response.writeUInt32BE(1,n+6);response.writeUInt16BE(4,n+10);response.writeUInt32BE(ipv4(address),n+12);}
 return response;
}
function wireName(name){return Buffer.concat([...name.split('.').map(v=>Buffer.concat([Buffer.from([v.length]),Buffer.from(v)])),Buffer.from([0])]);}
function apexReply(packet,q,zone,address){
 if(q.name===zone&&(q.type===2||q.type===6)&&q.qclass===1){
  const ns=wireName('ns.'+zone);let data=ns;
  if(q.type===6){const times=Buffer.alloc(20);[Math.floor(Date.now()/1000),300,60,86400,1].forEach((v,i)=>times.writeUInt32BE(v>>>0,i*4));data=Buffer.concat([ns,wireName('hostmaster.'+zone),times]);}
  const response=Buffer.alloc(q.end+12+data.length);packet.copy(response,0,0,q.end);response.writeUInt16BE(0x8400|(packet.readUInt16BE(2)&0x100),2);response.writeUInt16BE(1,6);response.writeUInt16BE(0,8);response.writeUInt16BE(0,10);
  const n=q.end;response.writeUInt16BE(0xc00c,n);response.writeUInt16BE(q.type,n+2);response.writeUInt16BE(1,n+4);response.writeUInt32BE(60,n+6);response.writeUInt16BE(data.length,n+10);data.copy(response,n+12);return response;
 }
 if(q.name===zone||q.name==='ns.'+zone)return dnsReply(packet,q,address,true);
 return null;
}
function status(addresses,cidrs,overflow){if(!addresses.length||!cidrs.length)return 'not_checked';if(!addresses.every(a=>allowed(a,cidrs)))return 'fail';return overflow?'not_checked':'pass';}
function record(entry,key,address){if(entry[key].has(address))return;if(entry[key].size<16)entry[key].add(address);else entry[key+'Overflow']=true;}
export function assessNetwork(entry,config,srflx=[]) {
 const dns=[...entry.dns],stun=[...entry.stun],http=[...entry.http];
 let rtc=status(stun,config.egressCidrs,entry.stunOverflow);
 if(rtc==='pass'&&!stun.some(ip=>srflx.some(candidate=>ipv4(ip)===ipv4(candidate))))rtc='not_checked';
 return {dns:status(dns,config.dnsCidrs,entry.dnsOverflow),webRtc:rtc,httpEgress:status(http,config.egressCidrs,entry.httpOverflow),
  observed:{dnsResolvers:dns,stunSources:stun,httpSources:http},scope:'IPv4 challenge correlation only; no-packet observations are not proof of no leakage'};
}
const bind=(socket,port,host)=>new Promise((yes,no)=>{socket.once('error',no);socket.bind(port,host,()=>{socket.off('error',no);yes();});});
const closeSocket=socket=>new Promise(yes=>{try{socket.close(yes);}catch{yes();}});
export async function startProbeService(config) {
 const host=config.bindHost??'127.0.0.1';const zone=config.zone.toLowerCase();
 if(!/^(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z0-9-]+$/.test(zone)||ipv4(config.publicIPv4)===null)throw Error('Invalid zone or IPv4');
 if(!config.tls&&!['127.0.0.1','::1'].includes(host))throw Error('Public HTTP listener requires TLS');
 for(const cidr of [...config.dnsCidrs,...config.egressCidrs])allowed('127.0.0.1',[cidr]);
 const challenges=new Map();const dns=createSocket('udp4');const ttl=config.ttlMs??60000;let closed=false;let pending=0;
 const drop=async token=>{const entry=challenges.get(token);if(entry){challenges.delete(token);await closeSocket(entry.socket);}};
 const expire=setInterval(()=>{for(const [token,entry]of challenges)if(entry.until<Date.now())void drop(token);},1000);expire.unref();
 dns.on('message',(packet,peer)=>{const q=dnsQuestion(packet);if(!q)return;
  const apex=apexReply(packet,q,zone,config.publicIPv4);if(apex){dns.send(apex,peer.port,peer.address);return;}
  const entry=q.name.endsWith(`.${zone}`)?challenges.get(q.name.slice(0,-zone.length-1)):undefined;
  if(entry)record(entry,'dns',peer.address);
  dns.send(dnsReply(packet,q,config.publicIPv4,Boolean(entry)),peer.port,peer.address);
 });
 dns.on('error',()=>{});
 const handler=async(req,res)=>{
  res.setHeader('Access-Control-Allow-Origin','*');res.setHeader('Cache-Control','no-store');res.setHeader('Access-Control-Allow-Methods','POST,GET,OPTIONS');res.setHeader('Access-Control-Allow-Headers','Content-Type');res.setHeader('Content-Type','application/json');res.setHeader('Connection','close');
  const finish=(code,value)=>{res.writeHead(code).end(JSON.stringify(value));};
  try{
   if(req.method==='OPTIONS'){finish(204,{});return;}
   if(req.method==='POST'&&req.url==='/v1/challenge'){
    if(closed||challenges.size+pending>=64){finish(429,{error:'busy'});return;}
    pending++;
    try {
    const token=randomBytes(16).toString('hex');const socket=createSocket('udp4');socket.on('error',()=>{});
    const min=config.stunMin??40000,max=config.stunMax??40127;let bound=false;
    for(let attempt=0;attempt<16&&!bound;attempt++)try{await bind(socket,min===0?0:min+Math.floor(Math.random()*(max-min+1)),host);bound=true;}catch{}
    if(!bound){await closeSocket(socket);finish(503,{error:'no probe port'});return;}
    if(closed){await closeSocket(socket);finish(503,{error:'closed'});return;}
    const entry={socket,dns:new Set(),stun:new Set(),http:new Set(),until:Date.now()+ttl};challenges.set(token,entry);
    socket.on('message',(packet,peer)=>{const response=stunReply(packet,peer.address,peer.port);if(response){record(entry,'stun',peer.address);socket.send(response,peer.port,peer.address);}});
    finish(200,{token,hostname:`${token}.${zone}`,stunPort:socket.address().port});return;
    } finally {pending--;}
   }
   const ping=req.url==='/v1/ping'&&(req.headers.host??'').split(':')[0].endsWith(`.${zone}`);
   if(ping){const token=req.headers.host.split('.')[0];const entry=challenges.get(token);if(!entry){finish(404,{error:'expired'});return;}
    record(entry,'http',req.socket.remoteAddress);finish(200,{ok:true});return;}
   const token=req.url?.match(/^\/v1\/result\/([0-9a-f]{32})$/)?.[1];const entry=challenges.get(token);
   if(req.method==='POST'&&entry){let size=0;const chunks=[];for await(const chunk of req){size+=chunk.length;if(size>4096){finish(413,{error:'too large'});return;}chunks.push(chunk);}
    const body=JSON.parse(Buffer.concat(chunks).toString()||'{}');const srflx=Array.isArray(body.srflx)?body.srflx.filter(v=>typeof v==='string'&&ipv4(v)!==null).slice(0,32):[];
    finish(200,assessNetwork(entry,config,srflx));await drop(token);return;}
   finish(404,{error:'not found'});
  }catch{if(!res.headersSent)finish(400,{error:'invalid request'});else res.end();}
 };
 const api=config.tls?httpsServer(config.tls,handler):httpServer(handler);
 api.requestTimeout=10000;api.headersTimeout=10000;api.maxRequestsPerSocket=10;
 try{await bind(dns,config.dnsPort??53,host);await new Promise((yes,no)=>{api.once('error',no);api.listen(config.apiPort??443,host,yes);});}
 catch(error){clearInterval(expire);await closeSocket(dns);api.close();throw error;}
 return {apiPort:api.address().port,dnsPort:dns.address().port,close:async()=>{closed=true;clearInterval(expire);await Promise.all([...challenges.keys()].map(drop));await closeSocket(dns);api.closeAllConnections();await new Promise(yes=>api.close(yes));}};
}
if(process.argv[1]&&import.meta.url===pathToFileURL(resolve(process.argv[1])).href){
 const list=name=>(process.env[name]??'').split(',').map(v=>v.trim()).filter(Boolean);
 const service=await startProbeService({bindHost:process.env.BIND_HOST??'0.0.0.0',zone:process.env.PROBE_ZONE??'',publicIPv4:process.env.PUBLIC_IPV4??'',dnsCidrs:list('DNS_ALLOWED_CIDRS'),egressCidrs:list('EGRESS_ALLOWED_CIDRS'),tls:{key:await readFile(process.env.TLS_KEY),cert:await readFile(process.env.TLS_CERT)}});
 for(const signal of ['SIGINT','SIGTERM'])process.once(signal,async()=>{await service.close();process.exit(0);});
}
