// Opt-in, bounded TURN allocation probe. No media data is relayed.
import {createHash, createHmac, randomBytes, timingSafeEqual} from 'node:crypto';
import net from 'node:net';
import tls from 'node:tls';
import dgram from 'node:dgram';
import {writeFile} from 'node:fs/promises';

const cookie = Buffer.from('2112a442', 'hex');
const attr = (type, value) => {
  const b = Buffer.alloc(4 + Math.ceil(value.length / 4) * 4);
  b.writeUInt16BE(type); b.writeUInt16BE(value.length, 2); value.copy(b, 4); return b;
};
function request(type, fields, key) {
  const id = randomBytes(12), body = Buffer.concat(fields), h = Buffer.alloc(20);
  h.writeUInt16BE(type); h.writeUInt16BE(body.length + (key ? 24 : 0), 2); cookie.copy(h, 4); id.copy(h, 8);
  const before = Buffer.concat([h, body]);
  return {id, bytes: key ? Buffer.concat([before, attr(8, createHmac('sha1', key).update(before).digest())]) : before};
}
function parse(bytes, id, key) {
  if (bytes.length < 20 || !bytes.subarray(4, 8).equals(cookie) || !bytes.subarray(8, 20).equals(id)) throw new Error('invalid_response');
  const end = 20 + bytes.readUInt16BE(2); if (end !== bytes.length || end > 4096) throw new Error('invalid_response');
  const fields = new Map(); let verified = false;
  for (let p = 20; p < end;) {
    if (p + 4 > end) throw new Error('invalid_response');
    const type = bytes.readUInt16BE(p), size = bytes.readUInt16BE(p + 2), next = p + 4 + Math.ceil(size / 4) * 4;
    if (next > end) throw new Error('invalid_response');
    fields.set(type, bytes.subarray(p + 4, p + 4 + size));
    if (type === 8 && key && size === 20) {
      const prefix = Buffer.from(bytes.subarray(0, p)); prefix.writeUInt16BE(p + 4, 2);
      verified = timingSafeEqual(createHmac('sha1', key).update(prefix).digest(), fields.get(8));
    }
    p = next;
  }
  return {type: bytes.readUInt16BE(0), fields, verified};
}
function mapped(value, id) {
  if (!value || ![8, 20].includes(value.length)) return null;
  const ip = Buffer.from(value.subarray(4)), mask = Buffer.concat([cookie, id]);
  if (value[1] === 1 && ip.length === 4) return [...ip].map((n, i) => n ^ mask[i]).join('.');
  if (value[1] === 2 && ip.length === 16) {
    for (let i = 0; i < 16; i++) ip[i] ^= mask[i];
    return Array.from({length: 8}, (_, i) => ip.readUInt16BE(i * 2).toString(16)).join(':');
  }
  return null;
}
async function probe(host, transport, port, username, credential) {
  const socket = transport === 'udp' ? dgram.createSocket('udp4') : transport === 'tls'
    ? tls.connect({host, port, servername: host, rejectUnauthorized: true}) : net.connect({host, port});
  const failure = new Promise((_, reject) => {
    socket.once('error', () => reject(new Error('connection_failed')));
    if (transport !== 'udp') socket.once('close', () => reject(new Error('connection_closed')));
  });
  let pending, buffered = Buffer.alloc(0);
  const consume = message => {
    if (!pending) return;
    if (transport === 'udp') { if (message.length <= 4096) pending(message); return; }
    buffered = Buffer.concat([buffered, message]);
    if (buffered.length > 4096) { pending(Buffer.alloc(0)); return; }
    if (buffered.length >= 20 && buffered.length >= 20 + buffered.readUInt16BE(2)) {
      const length = 20 + buffered.readUInt16BE(2), frame = buffered.subarray(0, length);
      buffered = buffered.subarray(length); pending(frame);
    }
  };
  socket.on(transport === 'udp' ? 'message' : 'data', consume);
  let timer;
  try {
    const work = async () => {
      if (transport !== 'udp') await new Promise(resolve => socket.once(transport === 'tls' ? 'secureConnect' : 'connect', resolve));
      const exchange = async (type, fields, key) => {
        const req = request(type, fields, key);
        const response = new Promise(resolve => { pending = resolve; });
        if (transport === 'udp') socket.send(req.bytes, port, host); else socket.write(req.bytes);
        return {id: req.id, ...parse(await response, req.id, key)};
      };
      const base = [attr(0x19, Buffer.from([17, 0, 0, 0])), attr(0x0d, Buffer.from([0, 0, 0, 60]))];
      const challenge = await exchange(3, base);
      const realm = challenge.fields.get(0x14), nonce = challenge.fields.get(0x15);
      if (challenge.type !== 0x113 || !realm || !nonce || realm.length > 128 || nonce.length > 256) throw new Error('auth_challenge_missing');
      const key = createHash('md5').update(`${username}:${realm.toString()}:${credential}`).digest();
      const auth = [attr(6, Buffer.from(username)), attr(0x14, realm), attr(0x15, nonce)];
      const allocation = await exchange(3, [...base, ...auth], key);
      const error = allocation.fields.get(9);
      if (allocation.type !== 0x103) return {status: 'not_checked', reason: 'allocation_rejected', errorCode: error?.length >= 4 ? error[2] * 100 + error[3] : null};
      if (!allocation.verified) throw new Error('response_integrity_failed');
      const clientIp = mapped(allocation.fields.get(0x20), allocation.id), relayIp = mapped(allocation.fields.get(0x16), allocation.id);
      if (!clientIp || !relayIp) throw new Error('allocation_addresses_missing');
      const refresh = request(4, [attr(0x0d, Buffer.alloc(4)), ...auth], key);
      if (transport === 'udp') socket.send(refresh.bytes, port, host); else socket.write(refresh.bytes);
      return {status: 'pass', clientIp, relayIp, integrityVerified: true, releaseRequested: true};
    };
    return await Promise.race([work(), failure, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('timeout')), 9000); })]);
  } catch (error) {
    const allowed = ['invalid_response','connection_failed','connection_closed','auth_challenge_missing','response_integrity_failed','allocation_addresses_missing','timeout'];
    return {status: 'not_checked', reason: allowed.includes(error.message) ? error.message : 'probe_failed'};
  } finally { clearTimeout(timer); if (transport === 'udp') { try { socket.close(); } catch {} } else socket.destroy(); }
}
const host = process.env.BROWS_TURN_HOST;
if (!host || !/^[a-z0-9][a-z0-9.-]{1,252}[a-z0-9]$/.test(host)) throw new Error('TURN hostname required');
let username = process.env.BROWS_TURN_USERNAME, credential = process.env.BROWS_TURN_CREDENTIAL;
if (process.env.BROWS_TURN_REST_SECRET) {
  username = `${Math.floor(Date.now() / 1000) + 900}:brows-acceptance`;
  credential = createHmac('sha1', process.env.BROWS_TURN_REST_SECRET).update(username).digest('base64');
}
if (!username || !credential) throw new Error('TURN credential configuration required');
for (const name of ['BROWS_TURN_USERNAME','BROWS_TURN_CREDENTIAL','BROWS_TURN_REST_SECRET']) delete process.env[name];
const report = {schemaVersion: 1, host, tests: await Promise.all([['udp',80],['tcp',80],['tls',443]].map(async ([transport,port]) => ({transport,port,...await probe(host,transport,port,username,credential)})))};
await writeFile(process.argv[2] ?? 'target/turn-preflight.json', JSON.stringify(report, null, 2));
console.log(JSON.stringify(report));
