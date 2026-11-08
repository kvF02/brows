import assert from 'node:assert/strict';
import test from 'node:test';
import {startLoginFixture, createProofPublisher} from './test_support/login_fixture.mjs';
import {mkdtemp, open, readFile, readdir, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';

test('proof snapshots remain immutable while an earlier snapshot is held open, including concurrent publications', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'brows-proof-test-'));
  let reader;
  try {
    const publish = createProofPublisher(directory);
    await publish({first: true});
    const first = path.join(directory, 'proofs-1.json');
    reader = await open(first, 'r');
    const snapshot = {first: true, second: true};
    const second = publish(snapshot);
    snapshot.second = false;
    await Promise.all([second, publish({first: true, second: true, third: true})]);
    assert.deepEqual(JSON.parse(await reader.readFile('utf8')), {first: true});
    assert.deepEqual(JSON.parse(await readFile(path.join(directory, 'proofs-2.json'))), {first: true, second: true});
    assert.deepEqual(JSON.parse(await readFile(path.join(directory, 'proofs-3.json'))), {first: true, second: true, third: true});
    assert.deepEqual((await readdir(directory)).sort(), ['proofs-1.json', 'proofs-2.json', 'proofs-3.json']);
  } finally { await reader?.close(); await rm(directory, {recursive: true, force: true}); }
});

test('the site acknowledges a proof only after publication and returns failure when its publisher fails', async () => {
  let release;
  const pending = new Promise(resolve => { release = resolve; });
  let publishing;
  const started = new Promise(resolve => { publishing = resolve; });
  let fail = false;
  const site = await startLoginFixture({onProof: async () => { publishing(); if (fail) throw Error('fixture write failed'); await pending; }});
  try {
    const origin = site.origin.replace('localhost', '127.0.0.1');
    let acknowledged = false;
    const response = fetch(origin + '/closed/wait/a', {method: 'POST'}).then(r => { acknowledged = true; return r; });
    await started;
    assert.equal(acknowledged, false);
    release();
    assert.equal((await response).status, 200);
    fail = true;
    assert.equal((await fetch(origin + '/proof/failure/a', {method: 'POST', body: '{}'})).status, 500);
  } finally { release(); await site.close(); }
});

test('read-only checks never silently authenticate a profile; enrolled cookies are secure and HttpOnly', async () => {
  const site = await startLoginFixture();
  try {
    const origin = site.origin.replace('localhost','127.0.0.1');
    const check = await fetch(origin+'/check/empty/a');
    assert.equal(check.headers.get('set-cookie'),null);
    const login = await fetch(origin+'/enroll/enroll/a');
    const cookie = login.headers.get('set-cookie');
    assert.match(cookie,/^__Host-brows-fixture=[a-f0-9]{64}; Path=\/; Max-Age=86400; Secure; HttpOnly; SameSite=Strict$/);
    const body = {local:'a',indexed:'a',cache:'a',httpOnly:true,secureContext:true};
    await fetch(origin+'/proof/enroll/a',{method:'POST',headers:{Cookie:cookie.split(';')[0]},body:JSON.stringify(body)});
    assert.equal(site.proofs['enroll/a'].account,'a');
    assert.equal(JSON.stringify(site.proofs).includes(cookie.split(';')[0]),false);
    await fetch(origin+'/proof/unauthenticated/b',{method:'POST',body:JSON.stringify({...body,local:null,indexed:null,cache:null})});
    assert.equal(site.proofs['unauthenticated/b'].account,null);
  } finally { await site.close(); }
});

test('proofs derive authentication from the request cookie, reject malformed storage and restrict routes', async () => {
  const site = await startLoginFixture();
  try {
    const origin = site.origin.replace('localhost','127.0.0.1');
    const spoof = await fetch(origin+'/proof/spoof/a',{method:'POST',body:JSON.stringify({account:'a',local:'a',indexed:'a',cache:'a',httpOnly:true,secureContext:true})});
    assert.equal(spoof.status,200); assert.equal(site.proofs['spoof/a'].account,null);
    const invalid = await fetch(origin+'/proof/invalid/a',{method:'POST',body:JSON.stringify({local:'arbitrary data'})});
    assert.equal(invalid.status,400); assert.equal(site.proofs['invalid/a'],undefined);
    assert.equal((await fetch(origin+'/enroll/invalid/c')).status,404);
    assert.equal((await fetch(origin+'/proof/large/a',{method:'POST',body:'x'.repeat(5000)})).status,413);
  } finally { await site.close(); }
});
