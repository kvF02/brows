import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { once } from "node:events";
import { createServer } from "node:http";
import net from "node:net";
import test from "node:test";
import { promisify } from "node:util";

const executable = process.env.BROWS_TEST_PROXY_PROBE;
const username = "fixture-user";
const password = 'fixture-"p\\ass:word';
async function runProbe(scheme, port, includePassword = true) {
  const env = { ...process.env };
  for (const key of ["BROWS_PROXY_ACCEPTANCE_USERNAME", "BROWS_PROXY_ACCEPTANCE_PASSWORD", "BROWS_PROXY_PROBE_USERNAME", "BROWS_PROXY_PROBE_PASSWORD"]) delete env[key];
  env.BROWS_PROXY_PROBE_USERNAME = username;
  if (includePassword) env.BROWS_PROXY_PROBE_PASSWORD = password;
  try {
    const result = await promisify(execFile)(executable, [scheme, "127.0.0.1", String(port)], { env, timeout: 10000, maxBuffer: 8192 });
    return { code: 0, ...result };
  } catch (error) {
    return { code: error.code, stdout: error.stdout ?? "", stderr: error.stderr ?? "" };
  }
}
function assertRedacted(result) {
  for (const value of [username, password, Buffer.from(username + ":" + password).toString("base64")]) {
    assert.ok(!result.stdout.includes(value) && !result.stderr.includes(value), "Probe output must not expose credentials");
  }
}

test("probe environment credentials reach HTTP CONNECT authentication", { skip: !executable }, async context => {
  const observed = [];
  const server = createServer((_req, res) => res.writeHead(405).end());
  server.on("connect", (req, socket) => {
    observed.push(req.headers["proxy-authorization"]);
    socket.end("HTTP/1.1 407 Proxy Authentication Required\r\nConnection: close\r\n\r\n");
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  context.after(() => new Promise(resolve => server.close(resolve)));
  const result = await runProbe("http", server.address().port);
  assert.equal(result.code, 1);
  assert.match(result.stdout, /^ERROR\|ProxyAuth\|/);
  assert.deepEqual(observed, ["Basic " + Buffer.from(username + ":" + password).toString("base64")]);
  assertRedacted(result);
});

test("probe environment credentials reach SOCKS5 authentication", { skip: !executable }, async context => {
  let observed;
  let failure;
  const sockets = new Set();
  const server = net.createServer(socket => {
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
    socket.on("error", () => {});
    (async () => {
      const chunks = socket[Symbol.asyncIterator]();
      let buffered = Buffer.alloc(0);
      async function read(count) {
        while (buffered.length < count) {
          const next = await chunks.next();
          if (next.done) throw new Error("Truncated SOCKS5 fixture request");
          buffered = Buffer.concat([buffered, next.value]);
        }
        const result = buffered.subarray(0, count);
        buffered = buffered.subarray(count);
        return result;
      }
      assert.deepEqual(await read(3), Buffer.from([5, 1, 2]));
      socket.write(Buffer.from([5, 2]));
      const header = await read(2);
      assert.equal(header[0], 1);
      const user = await read(header[1]);
      const length = await read(1);
      const secret = await read(length[0]);
      observed = { username: user.toString(), password: secret.toString() };
      socket.end(Buffer.from([1, 1]));
    })().catch(error => { failure = error; socket.destroy(); });
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  context.after(() => {
    for (const socket of sockets) socket.destroy();
    return new Promise(resolve => server.close(resolve));
  });
  const result = await runProbe("socks5", server.address().port);
  if (failure) throw failure;
  assert.equal(result.code, 1);
  assert.match(result.stdout, /^ERROR\|ProxyAuth\|/);
  assert.deepEqual(observed, { username, password });
  assertRedacted(result);
});

test("partial probe credentials fail before any network check", { skip: !executable }, async () => {
  const result = await runProbe("http", 1, false);
  assert.equal(result.code, 2);
  assert.equal(result.stdout, "");
  assertRedacted(result);
});
